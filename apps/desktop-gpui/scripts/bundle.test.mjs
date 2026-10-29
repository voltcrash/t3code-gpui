/* eslint-disable t3code/no-global-process-runtime -- Standalone Node packaging tools have no Effect runtime. */
import * as NodeAssert from "node:assert/strict";
import * as NodeFSP from "node:fs/promises";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeTest from "node:test";
import { bundle } from "./bundle.mjs";

async function fixture(run) {
  const root = await NodeFSP.mkdtemp(NodePath.join(NodeOS.tmpdir(), "t3-gpui-bundle-test-"));
  try {
    const put = async (name, content = "fixture") => {
      const file = NodePath.join(root, name);
      await NodeFSP.mkdir(NodePath.dirname(file), { recursive: true });
      await NodeFSP.writeFile(file, content);
      return file;
    };
    const desktop = await put("inputs/desktop");
    const serverRoot = NodePath.join(root, "server");
    const nodeRoot = NodePath.join(root, "node");
    await put("server/apps/server/dist/bin.mjs", "server fixture");
    await put("server/apps/server/dist/client/index.html", "web client fixture");
    await put("server/node_modules/native/addon.node", "native addon fixture");
    await put("node/bin/node", "node fixture");
    await put("node/node.exe", "windows node fixture");
    await put("node/LICENSE", "node license fixture");
    await run({ root, put, desktop, serverRoot, nodeRoot, version: "1.2.3-preview.1" });
  } finally {
    await NodeFSP.rm(root, { recursive: true, force: true });
  }
}

NodeTest.test(
  "portable resources survive relocation and keep the external web client",
  async () => {
    await fixture(async (input) => {
      await input.put("server/.env", "not a runtime asset");
      await input.put("server/userdata/statev2.sqlite", "not a runtime asset");
      const original = NodePath.join(input.root, "T3 Code");
      await bundle({ ...input, output: original, platform: "linux" });
      const output = NodePath.join(input.root, "Relocated T3 Code");
      await NodeFSP.rename(original, output);
      await NodeFSP.rm(input.serverRoot, { recursive: true });
      await NodeFSP.rm(input.nodeRoot, { recursive: true });
      await NodeFSP.rm(input.desktop);
      NodeAssert.equal(
        await NodeFSP.readFile(
          NodePath.join(output, "resources/server/apps/server/dist/client/index.html"),
          "utf8",
        ),
        "web client fixture",
      );
      NodeAssert.equal(
        await NodeFSP.readFile(
          NodePath.join(output, "resources/server/node_modules/native/addon.node"),
          "utf8",
        ),
        "native addon fixture",
      );
      NodeAssert.equal(
        await NodeFSP.readFile(NodePath.join(output, "resources/node/bin/node"), "utf8"),
        "node fixture",
      );
      NodeAssert.equal(
        await NodeFSP.readFile(NodePath.join(output, "resources/node/LICENSE"), "utf8"),
        "node license fixture",
      );
      NodeAssert.equal(
        await NodeFSP.stat(NodePath.join(output, "resources/server/.env")).catch(() => null),
        null,
      );
      NodeAssert.equal(
        await NodeFSP.stat(NodePath.join(output, "resources/server/userdata")).catch(() => null),
        null,
      );
    });
  },
);

NodeTest.test("macOS and Windows layouts match the native launcher's resource paths", async () => {
  await fixture(async (input) => {
    const mac = NodePath.join(input.root, "T3 Code.app");
    await bundle({ ...input, output: mac, platform: "darwin" });
    NodeAssert.ok((await NodeFSP.stat(NodePath.join(mac, "Contents/MacOS/t3-desktop"))).isFile());
    NodeAssert.match(
      await NodeFSP.readFile(NodePath.join(mac, "Contents/Info.plist"), "utf8"),
      /<string>1\.2\.3<\/string>/,
    );
    NodeAssert.ok(
      (await NodeFSP.stat(NodePath.join(mac, "Contents/Resources/node/bin/node"))).isFile(),
    );
    const win = NodePath.join(input.root, "windows");
    await bundle({ ...input, output: win, platform: "win32" });
    NodeAssert.equal(
      await NodeFSP.readFile(NodePath.join(win, "resources/node/node.exe"), "utf8"),
      "windows node fixture",
    );
    NodeAssert.ok((await NodeFSP.stat(NodePath.join(win, "t3-desktop.exe"))).isFile());
  });
});

NodeTest.test(
  "incomplete input, existing outputs, and input overlap leave inputs untouched",
  async () => {
    await fixture(async (input) => {
      const output = NodePath.join(input.root, "existing");
      await input.put("existing/keep.txt", "keep me");
      await NodeAssert.rejects(
        bundle({ ...input, output, platform: "linux" }),
        /Output already exists/,
      );
      NodeAssert.equal(
        await NodeFSP.readFile(NodePath.join(output, "keep.txt"), "utf8"),
        "keep me",
      );
      await NodeAssert.rejects(
        bundle({ ...input, output: NodePath.join(input.serverRoot, "output"), platform: "linux" }),
        /outside the input/,
      );
      await NodeFSP.rm(NodePath.join(input.serverRoot, "apps/server/dist/client/index.html"));
      const missing = NodePath.join(input.root, "missing");
      await NodeAssert.rejects(
        bundle({ ...input, output: missing, platform: "linux" }),
        /Missing file/,
      );
      NodeAssert.equal(await NodeFSP.stat(missing).catch(() => null), null);
    });
  },
);

NodeTest.test(
  "symlinked dependency trees fail atomically instead of baking in a workspace",
  async () => {
    await fixture(async (input) => {
      const target = await input.put("outside-package/index.js");
      await NodeFSP.symlink(
        NodePath.dirname(target),
        NodePath.join(input.serverRoot, "node_modules/leak"),
        NodeOS.platform() === "win32" ? "junction" : "dir",
      );
      const output = NodePath.join(input.root, "bundle");
      await NodeAssert.rejects(
        bundle({ ...input, output, platform: "linux" }),
        /physical production dependency tree/,
      );
      NodeAssert.equal(await NodeFSP.stat(output).catch(() => null), null);
      NodeAssert.ok(
        !(await NodeFSP.readdir(input.root)).some((name) => name.startsWith(".t3-gpui-bundle-")),
      );
    });
  },
);
