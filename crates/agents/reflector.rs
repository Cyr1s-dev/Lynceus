//! 确定性失败复盘器 —— `server/core/agents/reflector.py` 的移植。
//!
//! Reflector 不做修复：它把 task/tool 失败分类成结构化
//! [`ReflectorReport`]（失败类别、根因摘要、经验教训、playbook 更新建议），
//! 由 manager 持久化供知识沉淀。MVP 全部是确定性文本规则分类。

use models::{
    AgentTask, Observation, ReflectorFailureType, ReflectorReport, ToolInvocation, ToolStatus,
};

/// 失败分类与结构化教训产出者（`Reflector`）。
#[derive(Debug, Default)]
pub struct Reflector;

/// [`Reflector::reflect_failure`] 的输入参数。
pub struct ReflectFailureInput<'a> {
    /// 所属 Project。
    pub project_id: &'a str,
    /// 所属 Run。
    pub run_id: &'a str,
    /// 失败的 Task（可为 `None`，Python 默认）。
    pub task: Option<&'a AgentTask>,
    /// 失败前产出的 `ToolInvocation` 记录。
    pub tool_invocations: &'a [ToolInvocation],
    /// 关联 Observation。
    pub observations: &'a [Observation],
    /// 触发复盘的错误文本。
    pub error: Option<&'a str>,
}

impl Reflector {
    /// 分类 task/tool 失败并产出结构化报告。
    #[must_use]
    pub fn reflect_failure(&self, input: &ReflectFailureInput<'_>) -> ReflectorReport {
        let tools = input.tool_invocations;
        let obs = input.observations;
        let task_error = input
            .task
            .and_then(|task| task.error.as_deref())
            .unwrap_or("");
        let tool_text = tools
            .iter()
            .map(|tool| {
                let detail = tool.error.as_deref().unwrap_or("");
                if detail.is_empty() {
                    tool.output_summary.as_str()
                } else {
                    detail
                }
            })
            .collect::<Vec<&str>>()
            .join(" ");
        let error_text = input.error.unwrap_or("");
        let text = format!("{error_text} {task_error} {tool_text}").to_lowercase();

        let failure_type = classify_failure(&text, tools);
        let root_cause = root_cause(failure_type, input.task, input.error);
        let lessons = lessons_for(failure_type);
        let updates = playbook_updates(failure_type);

        let mut report = ReflectorReport::new(
            input.project_id.to_string().into(),
            input.run_id.to_string().into(),
        );
        report.task_id = input.task.map(|task| task.id.clone());
        report.failure_type = failure_type;
        report.root_cause_summary = root_cause;
        report.failure_modes = vec![failure_type.as_str().to_string()];
        report.lessons = lessons;
        report.suggested_playbook_updates.clone_from(&updates);
        report.related_observation_ids = obs
            .iter()
            .map(|item| item.id.as_str().to_string())
            .collect();
        report.outcome = "failed".to_string();
        report.recommended_playbooks = updates;
        report
    }
}

fn classify_failure(text: &str, tools: &[ToolInvocation]) -> ReflectorFailureType {
    if tools.iter().any(|tool| tool.status == ToolStatus::Timeout) {
        return ReflectorFailureType::Timeout;
    }
    if text.contains("timeout") || text.contains("timed out") {
        return ReflectorFailureType::Timeout;
    }
    if text.contains("invalid config") || text.contains("configuration") || text.contains("config[")
    {
        return ReflectorFailureType::InvalidConfig;
    }
    if text.contains("unsupported") {
        return ReflectorFailureType::UnsupportedTarget;
    }
    if text.contains("insufficient context") || text.contains("missing context") {
        return ReflectorFailureType::InsufficientContext;
    }
    let unavailable_markers = [
        "not found",
        "not installed",
        "unavailable",
        "no such file",
        "not recognized",
        "denied",
    ];
    if unavailable_markers
        .iter()
        .any(|marker| text.contains(marker))
    {
        return ReflectorFailureType::ToolUnavailable;
    }
    if !tools.is_empty()
        || text.contains("solver")
        || text.contains("exception")
        || text.contains("error")
    {
        return ReflectorFailureType::SolverError;
    }
    ReflectorFailureType::Unknown
}

fn root_cause(
    failure_type: ReflectorFailureType,
    task: Option<&AgentTask>,
    error: Option<&str>,
) -> String {
    let task_part = match task {
        Some(task) => format!(" task {}", task.id.as_str()),
        None => String::new(),
    };
    let task_error = task.and_then(|item| item.error.as_deref());
    // Python `error or (task.error if task and task.error else "")`：空串视同缺省。
    let detail = error
        .filter(|value| !value.is_empty())
        .or_else(|| task_error.filter(|value| !value.is_empty()));
    match detail {
        Some(detail) => format!("{}{task_part}: {detail}", failure_type.as_str()),
        None => format!(
            "{}{task_part}: deterministic classification only",
            failure_type.as_str()
        ),
    }
}

fn lessons_for(failure_type: ReflectorFailureType) -> Vec<String> {
    match failure_type {
        ReflectorFailureType::ToolUnavailable => {
            vec!["Verify tool registration and module health before dispatch.".to_string()]
        }
        ReflectorFailureType::InvalidConfig => {
            vec!["Validate run configuration before task creation.".to_string()]
        }
        ReflectorFailureType::Timeout => {
            vec![
                "Prefer narrower target slices or higher timeout budgets for heavy tools."
                    .to_string(),
            ]
        }
        ReflectorFailureType::InsufficientContext => {
            vec!["Build a ContextPack with origin, goal, and current intent facts.".to_string()]
        }
        ReflectorFailureType::UnsupportedTarget => {
            vec!["Check engine-domain compatibility before solver routing.".to_string()]
        }
        ReflectorFailureType::SolverError => {
            vec![
                "Persist failed ToolInvocations and retry only after narrowing the branch."
                    .to_string(),
            ]
        }
        ReflectorFailureType::Unknown => {
            vec![
                "Capture structured error data to improve deterministic classification."
                    .to_string(),
            ]
        }
    }
}

fn playbook_updates(failure_type: ReflectorFailureType) -> Vec<String> {
    vec![format!("reflector:{}:triage", failure_type.as_str())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::TaskStatus;

    fn task(error: Option<&str>) -> AgentTask {
        let mut task = AgentTask::new(
            "proj_1".to_string().into(),
            "run_1".to_string().into(),
            "solver_x".to_string(),
        );
        task.status = TaskStatus::Failed;
        task.error = error.map(str::to_string);
        task
    }

    fn invocation(status: ToolStatus, error: Option<&str>) -> ToolInvocation {
        let mut inv = ToolInvocation::new("nuclei".to_string(), "scan target".to_string());
        inv.status = status;
        inv.error = error.map(str::to_string);
        inv
    }

    #[test]
    fn classifies_timeout_from_tool_status() {
        let tools = vec![invocation(ToolStatus::Timeout, Some("boom"))];
        let report = Reflector.reflect_failure(&ReflectFailureInput {
            project_id: "proj_1",
            run_id: "run_1",
            task: None,
            tool_invocations: &tools,
            observations: &[],
            error: None,
        });
        assert_eq!(report.failure_type, ReflectorFailureType::Timeout);
        assert_eq!(report.failure_modes, ["timeout"]);
        assert_eq!(report.outcome, "failed");
        assert_eq!(report.recommended_playbooks, ["reflector:timeout:triage"]);
    }

    #[test]
    fn classifies_tool_unavailable_from_text() {
        let report = Reflector.reflect_failure(&ReflectFailureInput {
            project_id: "proj_1",
            run_id: "run_1",
            task: Some(&task(Some("ffuf not found in PATH"))),
            tool_invocations: &[],
            observations: &[],
            error: None,
        });
        assert_eq!(report.failure_type, ReflectorFailureType::ToolUnavailable);
        assert!(report.root_cause_summary.contains("task "));
        assert!(report.root_cause_summary.contains("ffuf not found in PATH"));
    }

    #[test]
    fn classifies_solver_error_and_unknown() {
        let tools = vec![invocation(ToolStatus::Error, Some("exit 1"))];
        let report = Reflector.reflect_failure(&ReflectFailureInput {
            project_id: "proj_1",
            run_id: "run_1",
            task: Some(&task(Some("ValueError: bad"))),
            tool_invocations: &tools,
            observations: &[],
            error: None,
        });
        assert_eq!(report.failure_type, ReflectorFailureType::SolverError);

        let report = Reflector.reflect_failure(&ReflectFailureInput {
            project_id: "proj_1",
            run_id: "run_1",
            task: None,
            tool_invocations: &[],
            observations: &[],
            error: None,
        });
        assert_eq!(report.failure_type, ReflectorFailureType::Unknown);
        assert_eq!(
            report.root_cause_summary,
            "unknown: deterministic classification only"
        );
    }

    #[test]
    fn records_task_and_observation_links() {
        let task = task(Some("config[tools] malformed"));
        let observations = [Observation::new(
            "proj_1".to_string().into(),
            "run_1".to_string().into(),
            "s".to_string(),
        )];
        let report = Reflector.reflect_failure(&ReflectFailureInput {
            project_id: "proj_1",
            run_id: "run_1",
            task: Some(&task),
            tool_invocations: &[],
            observations: &observations,
            error: None,
        });
        assert_eq!(
            report.task_id.as_ref().map(models::TaskId::as_str),
            Some(task.id.as_str())
        );
        assert_eq!(report.related_observation_ids.len(), 1);
        assert_eq!(report.failure_type, ReflectorFailureType::InvalidConfig);
    }
}
