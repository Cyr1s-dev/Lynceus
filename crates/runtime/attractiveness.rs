//! 寻宝机制:分支吸引子分数(参考 StrikeAgent 螺旋/`est_success`,适配 Lynceus)。
//!
//! 波次内分支取舍用:高吸引力分支优先拿到步数预算与并发槽(见
//! [`crate::branch_runtime`] 的 `run_branch_runtime`——它对 `runnable` /
//! `admitted` 按本模块降序排后再入波)。分数**只用分支自身已持久化的图信号**
//! (`priority`、关联发现/证据、预算余量),不额外查库:波次排序无 IO、确定、可测。
//!
//! 与 StrikeAgent 的对应:它的 Intent 带 `est_success`(预估成功率)按高分先派,
//! 这里用分支的图信号算同等意义的"吸引子";后续阶段在此叠加停滞升圈与命中
//! 正反馈(见模块测试与 `run_branch_runtime` 的螺旋账本)。

use models::Branch;
use serde_json::{Map, Value, json};

/// `run.config` 里记录螺旋停滞计数的键(无需 schema 迁移)。
pub const SPIRAL_EMPTY_KEY: &str = "spiral_empty_passes";

/// 记录一次波次产出:productive 清零,否则 +1。返回新的空转计数。
///
/// 对应 StrikeAgent 螺旋账本的 `empty_plans`:有增长清零、空转累加,
/// 是"停滞才扩张"的驱动量。
pub fn note_wave_productivity(config: &mut Map<String, Value>, productive: bool) -> i64 {
    let current = empty_passes(config);
    let next = if productive { 0 } else { current + 1 };
    config.insert(SPIRAL_EMPTY_KEY.to_string(), json!(next));
    next
}

/// 从 `run.config` 读当前空转计数。
#[must_use]
pub fn empty_passes(config: &Map<String, Value>) -> i64 {
    config
        .get(SPIRAL_EMPTY_KEY)
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0)
}

/// 由空转计数推导并发扩张档:每 3 轮空转 +1,封顶 +3。
///
/// 停滞越久,swarm 多派引擎(探索越宽)——即"当前攻击面挖空了才扩张",
/// 不早滥炸也不卡死。仅加在用户配置的 `intent_worker_concurrency` 之上,
/// 不覆盖用户设置。
#[must_use]
pub fn escalation_bump(empty_passes: i64) -> usize {
    (empty_passes / 3).clamp(0, 3) as usize
}

/// 计算分支吸引子分数(0-100)。
///
/// - 基分 = `branch.priority`(规划/用户意图,约束 `[0,100]`)。
/// - 肥沃区正反馈:关联已确认发现 → `+8`(出过洞的区域更值得深挖,寻宝梯度)。
/// - 有证据链 → `+3`。
/// - 预算将枯(`steps_used` ≥ 80% `budget_steps`)→ `-10`(不在将枯分支上空转)。
#[must_use]
pub fn branch_attractiveness(branch: &Branch) -> f64 {
    let mut score = branch.priority.clamp(0, 100) as f64;
    if !branch.related_finding_ids.is_empty() {
        score += 8.0;
    }
    if !branch.related_evidence_ids.is_empty() {
        score += 3.0;
    }
    if branch.budget_steps > 0 {
        let used_ratio = branch.steps_used as f64 / branch.budget_steps as f64;
        if used_ratio >= 0.8 {
            score -= 10.0;
        }
    }
    score.clamp(0.0, 100.0)
}

/// 按吸引子降序**稳定**排序分支(高分先派;同分保持原创建序,确定性可测)。
#[must_use]
pub fn rank_by_attractiveness(mut branches: Vec<Branch>) -> Vec<Branch> {
    branches.sort_by(|a, b| {
        branch_attractiveness(b)
            .partial_cmp(&branch_attractiveness(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    branches
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::ids::{MissionId, ProjectId};

    fn branch(priority: i64) -> Branch {
        let mut branch = Branch::new(
            ProjectId::new("p".to_string()),
            MissionId::new("m".to_string()),
            "t".to_string(),
            "h".to_string(),
        );
        branch.priority = priority;
        branch
    }

    #[test]
    fn priority_is_the_base_score() {
        assert!((branch_attractiveness(&branch(70)) - 70.0).abs() < 1e-9);
        // 越界的 priority 被夹到 [0,100]。
        assert!((branch_attractiveness(&branch(150)) - 100.0).abs() < 1e-9);
        assert!((branch_attractiveness(&branch(-5)) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn fertile_branches_outrank_bare_ones_at_equal_priority() {
        let mut fertile = branch(50);
        fertile.related_finding_ids = vec!["f1".to_string()];
        assert!(branch_attractiveness(&fertile) > branch_attractiveness(&branch(50)));
    }

    #[test]
    fn near_exhausted_branches_are_penalized() {
        let mut exhausted = branch(90);
        exhausted.budget_steps = 10;
        exhausted.steps_used = 9; // 90% used
        assert!(branch_attractiveness(&exhausted) < 90.0);
    }

    #[test]
    fn rank_orders_by_attractiveness_descending() {
        let mut rich = branch(40);
        rich.related_finding_ids = vec!["f".to_string()]; // 40 + 8 = 48
        let poor = branch(45); // 45
        let ranked = rank_by_attractiveness(vec![poor, rich]);
        assert_eq!(ranked[0].priority, 40, "fertile 48 must come before bare 45");
        assert_eq!(ranked[1].priority, 45);
    }

    #[test]
    fn score_is_clamped_to_0_100() {
        let mut maxed = branch(100);
        maxed.related_finding_ids = vec!["f".to_string()];
        maxed.related_evidence_ids = vec!["e".to_string()];
        assert!((branch_attractiveness(&maxed) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn equal_scores_keep_stable_creation_order() {
        let a = branch(60);
        let b = branch(60);
        let ranked = rank_by_attractiveness(vec![a, b]);
        // 稳定排序:同分不重排(这里仅验证不 panic 且数量不变)。
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn spiral_empty_passes_reset_on_progress_and_accumulate_on_stall() {
        let mut config = serde_json::Map::new();
        assert_eq!(note_wave_productivity(&mut config, false), 1);
        assert_eq!(note_wave_productivity(&mut config, false), 2);
        assert_eq!(note_wave_productivity(&mut config, true), 0);
        assert_eq!(note_wave_productivity(&mut config, false), 1);
        assert_eq!(empty_passes(&config), 1);
    }

    #[test]
    fn escalation_bump_grows_with_stall_and_is_capped() {
        assert_eq!(escalation_bump(0), 0usize);
        assert_eq!(escalation_bump(2), 0usize);
        assert_eq!(escalation_bump(3), 1usize);
        assert_eq!(escalation_bump(9), 3usize);
        assert_eq!(escalation_bump(30), 3usize); // 封顶 +3
    }
}
