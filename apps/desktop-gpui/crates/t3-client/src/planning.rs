//! Client presentation of the existing Plan/Build and proposed-plan workflow.

use crate::providers::Provider;
use crate::thread::{Plan, ThreadProjection};

pub fn enabled(feature_enabled: bool, provider: Option<&Provider>) -> bool {
    feature_enabled
        && provider.is_some_and(|provider| provider.show_interaction_mode_toggle != Some(false))
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProposedPlan {
    pub id: String,
    pub markdown: String,
}

/// Matches findLatestProposedPlan: prefer the latest run's proposal, otherwise
/// the newest proposal. A newer completed/superseded plan must hide an older active one.
pub fn active_plan(projection: &ThreadProjection) -> Option<ProposedPlan> {
    let latest_run = projection
        .runs
        .iter()
        .max_by_key(|run| run.ordinal.unwrap_or(0));
    if latest_run.is_some_and(|run| {
        !matches!(
            run.status.as_str(),
            "completed" | "failed" | "interrupted" | "cancelled" | "rolled_back"
        )
    }) {
        return None;
    }
    let plans: Vec<_> = projection
        .plans
        .iter()
        .filter(|plan| plan.kind == "proposed_plan")
        .collect();
    let in_latest_run = latest_run.is_some_and(|run| {
        plans
            .iter()
            .any(|plan| plan.run_id.as_deref() == Some(&run.id))
    });
    let plan = plans
        .into_iter()
        .filter(|plan| !in_latest_run || plan.run_id.as_ref() == latest_run.map(|run| &run.id))
        .max_by(|left, right| {
            plan_time(projection, left)
                .cmp(plan_time(projection, right))
                .then_with(|| left.id.cmp(&right.id))
        })?;
    if plan.status.as_deref() != Some("active") {
        return None;
    }
    let item = projection
        .turn_items
        .iter()
        .rev()
        .find(|item| item.kind == "proposed_plan" && item.str("planId") == Some(&plan.id));
    if item.is_some_and(|item| {
        item.fields
            .get("streaming")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    }) {
        return None;
    }
    let markdown = item
        .and_then(|item| item.str("markdown"))
        .or(plan.markdown.as_deref())?
        .trim();
    (!markdown.is_empty()).then(|| ProposedPlan {
        id: plan.id.clone(),
        markdown: markdown.into(),
    })
}

fn plan_time<'a>(projection: &'a ThreadProjection, plan: &Plan) -> &'a str {
    projection
        .turn_items
        .iter()
        .rev()
        .find(|item| item.kind == "proposed_plan" && item.str("planId") == Some(&plan.id))
        .map(|item| item.updated_at.as_str())
        .or_else(|| {
            projection
                .runs
                .iter()
                .find(|run| Some(&run.id) == plan.run_id.as_ref())
                .and_then(|run| {
                    run.completed_at
                        .as_deref()
                        .or(run.started_at.as_deref())
                        .or(run.requested_at.as_deref())
                })
        })
        .or_else(|| {
            projection
                .thread
                .get("createdAt")
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or("")
}

pub fn implementation_prompt(plan: &ProposedPlan) -> String {
    format!("PLEASE IMPLEMENT THIS PLAN:\n{}", plan.markdown.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn provider(toggle: Option<bool>) -> Provider {
        serde_json::from_value(json!({"instanceId":"custom", "driver":"opencode", "enabled":true, "status":"ready", "showInteractionModeToggle":toggle})).unwrap()
    }
    #[test]
    fn plan_mode_follows_feature_and_provider_capabilities() {
        for toggle in [None, Some(true), Some(false)] {
            let provider = provider(toggle);
            assert_eq!(enabled(true, Some(&provider)), toggle != Some(false));
            assert!(!enabled(false, Some(&provider)));
        }
        assert!(!enabled(true, None));
    }
    fn projection() -> ThreadProjection {
        serde_json::from_value(json!({
            "thread":{"id":"thread", "createdAt":"2026-10-26"},
            "runs":[{"id":"run", "status":"completed", "ordinal":1}], "attempts":[], "runtimeRequests":[],
            "plans":[{"id":"plan", "kind":"proposed_plan", "status":"active", "runId":"run", "detailInTurnItem":true}],
            "turnItems":[{"id":"item", "threadId":"thread", "runId":"run", "ordinal":1, "status":"completed", "type":"proposed_plan", "updatedAt":"2026-10-26", "planId":"plan", "markdown":"# Plan\n\n1. Make the change.", "streaming":false}],
            "visibleTurnItems":[]
        })).unwrap()
    }
    #[test]
    fn completed_local_proposals_use_the_turn_item_detail_and_keep_plan_identity() {
        let plan = active_plan(&projection()).unwrap();
        assert_eq!(plan.id, "plan");
        assert_eq!(
            implementation_prompt(&plan),
            "PLEASE IMPLEMENT THIS PLAN:\n# Plan\n\n1. Make the change."
        );
        let mut projection = projection();
        projection.plans[0].status = Some("completed".into());
        assert!(active_plan(&projection).is_none());
        projection.plans[0].status = Some("active".into());
        projection.runs[0].status = "running".into();
        assert!(active_plan(&projection).is_none());
        projection.runs[0].status = "completed".into();
        projection.turn_items[0]
            .fields
            .insert("streaming".into(), true.into());
        assert!(active_plan(&projection).is_none());
    }
    #[test]
    fn newer_resolved_plans_do_not_resurrect_older_active_plans() {
        let mut projection = projection();
        let mut newer = projection.plans[0].clone();
        newer.id = "plan-z".into();
        newer.status = Some("superseded".into());
        newer.markdown = Some("Other plan".into());
        projection.plans.push(newer);
        assert!(active_plan(&projection).is_none());
    }
}
