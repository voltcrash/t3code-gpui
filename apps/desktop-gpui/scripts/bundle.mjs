#!/usr/bin/env node
/* eslint-disable t3code/no-global-process-runtime -- Standalone Node packaging tools have no Effect runtime. */
import * as NodeOS from "node:os";
// Assemble native desktop resources without depending on Electron's packager.
import * as NodeFSP from "node:fs/promises";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import * as NodeUtil from "node:util";

const appLicense = NodeURL.fileURLToPath(new URL("../../../LICENSE", import.meta.url));

async function requirePath(file, directory = false) {
  const info = await NodeFSP.stat(file).catch(() => null);
  if (!info || (directory ? !info.isDirectory() : !info.isFile())) {
    throw new Error(`Missing ${directory ? "directory" : "file"}: ${file}`);
  }
}

function inside(root, candidate) {
  const relative = NodePath.relative(root, candidate);
  return (
    relative === "" ||
    (!relative.startsWith(`..${NodePath.sep}`) &&
      relative !== ".." &&
      !NodePath.isAbsolute(relative))
  );
}

async function copyRuntime(source, destination) {
  await NodeFSP.cp(source, destination, {
    recursive: true,
    filter: async (file) => {
      // Runtime trees do not execute package-manager shims. Hoisted production
      // installs otherwise contain only physical files, including native addons.
      if (NodePath.basename(file) === ".bin") return false;
      if ((await NodeFSP.lstat(file)).isSymbolicLink()) {
        throw new Error(
          `Runtime tree contains a symlink: ${file}. Supply a physical production dependency tree.`,
        );
      }
      return true;
    },
  });
}

export async function bundle({
  desktop,
  serverRoot,
  nodeRoot,
  output,
  version,
  platform = NodeOS.platform(),
}) {
  if (!["linux", "darwin", "win32"].includes(platform))
    throw new Error(`Unsupported platform: ${platform}`);
  const releaseVersion = version?.match(/^(\d+\.\d+\.\d+)(?:[-+][\w.+-]+)?$/)?.[1];
  if (!releaseVersion) throw new Error("A version such as 0.1.0 is required.");
  let destination = NodePath.resolve(output);
  if (platform === "darwin" && !destination.endsWith(".app"))
    throw new Error("macOS output must end in .app.");
  if (destination === NodePath.dirname(destination))
    throw new Error("Output cannot be a filesystem root.");
  if (await NodeFSP.lstat(destination).catch(() => null))
    throw new Error(`Output already exists: ${destination}`);
  desktop = NodePath.resolve(desktop);
  serverRoot = await NodeFSP.realpath(serverRoot);
  nodeRoot = await NodeFSP.realpath(nodeRoot);
  if (inside(serverRoot, destination) || inside(nodeRoot, destination))
    throw new Error("Output must be outside the input runtime trees.");
  const serverDist = NodePath.join(serverRoot, "apps/server/dist");
  const node = NodePath.join(nodeRoot, platform === "win32" ? "node.exe" : "bin/node");
  await Promise.all([
    requirePath(desktop),
    requirePath(node),
    requirePath(NodePath.join(nodeRoot, "LICENSE")),
    requirePath(NodePath.join(serverDist, "bin.mjs")),
    requirePath(NodePath.join(serverDist, "client/index.html")),
    requirePath(NodePath.join(serverRoot, "node_modules"), true),
    requirePath(appLicense),
  ]);
  if (await NodeFSP.stat(NodePath.join(serverRoot, "node_modules/electron")).catch(() => null)) {
    throw new Error("The server runtime must not include Electron.");
  }
  await NodeFSP.mkdir(NodePath.dirname(destination), { recursive: true });
  destination = NodePath.join(
    await NodeFSP.realpath(NodePath.dirname(destination)),
    NodePath.basename(destination),
  );
  if (inside(serverRoot, destination) || inside(nodeRoot, destination))
    throw new Error("Output must be outside the input runtime trees.");
  const stage = await NodeFSP.mkdtemp(
    NodePath.join(NodePath.dirname(destination), ".t3-gpui-bundle-"),
  );
  try {
    const contents = platform === "darwin" ? NodePath.join(stage, "Contents") : stage;
    const resources = NodePath.join(contents, platform === "darwin" ? "Resources" : "resources");
    const executable = NodePath.join(
      contents,
      platform === "darwin"
        ? "MacOS/t3-desktop"
        : platform === "win32"
          ? "t3-desktop.exe"
          : "t3-desktop",
    );
    await NodeFSP.mkdir(NodePath.dirname(executable), { recursive: true });
    await NodeFSP.mkdir(NodePath.join(resources, "node"), { recursive: true });
    await NodeFSP.copyFile(desktop, executable);
    if (platform !== "win32") await NodeFSP.chmod(executable, 0o755);
    const bundledNode = NodePath.join(
      resources,
      "node",
      platform === "win32" ? "node.exe" : "bin/node",
    );
    await NodeFSP.mkdir(NodePath.dirname(bundledNode), { recursive: true });
    await NodeFSP.copyFile(node, bundledNode);
    if (platform !== "win32") await NodeFSP.chmod(bundledNode, 0o755);
    await NodeFSP.copyFile(
      NodePath.join(nodeRoot, "LICENSE"),
      NodePath.join(resources, "node/LICENSE"),
    );
    await NodeFSP.copyFile(appLicense, NodePath.join(resources, "LICENSE"));
    await copyRuntime(serverDist, NodePath.join(resources, "server/apps/server/dist"));
    await copyRuntime(
      NodePath.join(serverRoot, "node_modules"),
      NodePath.join(resources, "server/node_modules"),
    );
    if (platform === "darwin") {
      await NodeFSP.writeFile(
        NodePath.join(contents, "Info.plist"),
        `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>t3-desktop</string>
<key>CFBundleIdentifier</key><string>com.t3tools.t3code</string>
<key>CFBundleName</key><string>T3 Code</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>${releaseVersion}</string>
<key>CFBundleVersion</key><string>${releaseVersion}</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>\n`,
      );
    }
    await NodeFSP.rename(stage, destination);
    return destination;
  } finally {
    await NodeFSP.rm(stage, { recursive: true, force: true });
  }
}

async function main() {
  const { values } = NodeUtil.parseArgs({
    options: {
      desktop: { type: "string" },
      "server-root": { type: "string" },
      "node-root": { type: "string" },
      output: { type: "string" },
      version: { type: "string" },
      help: { type: "boolean" },
    },
  });
  if (values.help) {
    console.log(
      "node apps/desktop-gpui/scripts/bundle.mjs --desktop <binary> --server-root <prepared-runtime> --node-root <node-distribution> --output <new-directory> [--version <version>]",
    );
    return;
  }
  for (const key of ["desktop", "server-root", "node-root", "output"]) {
    if (!values[key]) throw new Error(`--${key} is required.`);
  }
  const cargo = await NodeFSP.readFile(
    new URL("../crates/t3-desktop/Cargo.toml", import.meta.url),
    "utf8",
  );
  const version = values.version ?? cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  console.log(
    await bundle({
      desktop: values.desktop,
      serverRoot: values["server-root"],
      nodeRoot: values["node-root"],
      output: values.output,
      version,
    }),
  );
}

if (
  process.argv[1] &&
  NodePath.resolve(process.argv[1]) === NodeURL.fileURLToPath(import.meta.url)
) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
