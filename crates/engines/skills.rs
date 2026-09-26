//! Skill 目录（WP6）——`skills/<name>/SKILL.md` 约定的文件管理器。
//!
//! 目录布局（默认 `data/skills`，`LYNCEUS_SKILLS_DIR` 覆盖）：
//!
//! ```text
//! skills/
//!   <name>/
//!     SKILL.md          # YAML frontmatter + 操作手册正文
//!     <附属文件…>        # 与手册同目录的脚本/清单等
//! ```
//!
//! frontmatter 字段：`name` / `description` 必填；`license` /
//! `compatibility` 可选；`modules` 是工具目录条目 id 的逗号分隔列表——
//! worker `skill_load` 该技能时**只解锁列出的条目**（fail-closed），经
//! broker 守卫链执行。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// skill 名约束：小写字母/数字/连字符（目录名即 id，防路径歧义）。
pub fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// SKILL.md frontmatter 元数据。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SkillMeta {
    /// skill 名（必须与目录名一致）。
    pub name: String,
    /// 一句话描述（模型经 skill_list 看到的选择依据）。
    pub description: String,
    /// 许可证（可选）。
    #[serde(default)]
    pub license: Option<String>,
    /// 兼容性说明（可选）。
    #[serde(default)]
    pub compatibility: Option<String>,
    /// 工具目录条目 id 列表（`modules:` 逗号分隔；skill_load 时解锁）。
    #[serde(default)]
    pub modules: Vec<String>,
}

impl SkillMeta {
    /// 从 frontmatter 文本解析（`---` 围栏内的 `key: value` 行）。
    #[must_use]
    pub fn parse(text: &str) -> Option<(Self, String)> {
        let rest = text.strip_prefix("---")?;
        let end = rest.find("\n---")?;
        let frontmatter = &rest[..end];
        let body = rest[end + 4..].trim_start_matches(['\n', '\r']).to_string();
        let mut meta = Self::default();
        for line in frontmatter.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "name" => meta.name = value.to_string(),
                "description" => meta.description = value.to_string(),
                "license" => meta.license = Some(value.to_string()),
                "compatibility" => meta.compatibility = Some(value.to_string()),
                "modules" => {
                    meta.modules = value
                        .split(',')
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(str::to_string)
                        .collect();
                }
                _ => {}
            }
        }
        if meta.name.is_empty() || meta.description.is_empty() {
            return None;
        }
        Some((meta, body))
    }
}

pub use models::skill::{SkillMissingEntry, SkillUsageRow};

/// skill 目录管理器（文件系统是权威源）。
pub struct SkillManager {
    root: PathBuf,
}

impl SkillManager {
    /// 从环境构造（`LYNCEUS_SKILLS_DIR`，缺省 `data/skills`）。
    #[must_use]
    pub fn from_env() -> Self {
        let root = std::env::var("LYNCEUS_SKILLS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data").join("skills"));
        Self { root }
    }

    /// 以显式根目录构造（测试用）。
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// 运行时替换根目录（测试注入；生产保持 `from_env` 的根）。
    pub fn set_root(&mut self, root: PathBuf) {
        self.root = root;
    }

    fn skill_dir(&self, name: &str) -> Option<PathBuf> {
        if !is_valid_skill_name(name) {
            return None;
        }
        Some(self.root.join(name))
    }

/// 列出全部 skill（按名排序；损坏的 SKILL.md 跳过并计数）。
///
/// 运行时根目录**不存在**时返回空列表而不是报错：全新 clone 上 `data/` 被
/// `.gitignore` 忽略、播种尚未执行，此刻 `/skills` 应当显示"暂无"而不是 500。
///
/// # Errors
/// 根目录存在但不可读。
pub fn list(&self) -> Result<Vec<SkillMeta>, String> {
    let mut skills = Vec::new();
    let entries = match fs::read_dir(&self.root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(skills),
        Err(error) => {
            return Err(format!("cannot read skill dir {:?}: {error}", self.root));
        }
    };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let skill_md = path.join("SKILL.md");
            let Ok(text) = fs::read_to_string(&skill_md) else {
                continue;
            };
            if let Some((meta, _)) = SkillMeta::parse(&text) {
                skills.push(meta);
            }
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(skills)
    }

    /// 读取一个 skill（元数据 + 正文）。
    ///
    /// # Errors
    /// 名字非法、目录或 SKILL.md 不存在、frontmatter 不完整。
    pub fn load(&self, name: &str) -> Result<(SkillMeta, String), String> {
        let dir = self
            .skill_dir(name)
            .ok_or_else(|| format!("invalid skill name '{name}'"))?;
        let text = fs::read_to_string(dir.join("SKILL.md"))
            .map_err(|error| format!("skill '{name}' has no readable SKILL.md: {error}"))?;
        SkillMeta::parse(&text).ok_or_else(|| format!("skill '{name}' has invalid frontmatter"))
    }

    /// skill 相对文件树（`/` 分隔；SKILL.md 在首）。
    ///
    /// # Errors
    /// 名字非法或目录不可读。
    pub fn file_tree(&self, name: &str) -> Result<Vec<String>, String> {
        let dir = self
            .skill_dir(name)
            .ok_or_else(|| format!("invalid skill name '{name}'"))?;
        let mut files = Vec::new();
        collect_files(&dir, dir.clone(), &mut files, 0)?;
        files.sort();
        Ok(files)
    }

    /// 读一个附属文件（路径不得越出 skill 目录）。
    ///
    /// # Errors
    /// 名字/路径非法或文件不可读。
    pub fn read_file(&self, name: &str, relative: &str) -> Result<String, String> {
        let path = self.resolve_inner(name, relative)?;
        fs::read_to_string(path).map_err(|error| format!("cannot read skill file: {error}"))
    }

    /// 写一个附属文件（SKILL.md 与附属文件均可；目录自动创建）。
    ///
    /// # Errors
    /// 名字/路径非法或写入失败。
    pub fn write_file(&self, name: &str, relative: &str, content: &str) -> Result<(), String> {
        let path = self.resolve_inner(name, relative)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create skill subdirectory: {error}"))?;
        }
        fs::write(path, content).map_err(|error| format!("cannot write skill file: {error}"))
    }

    /// 新建 skill（SKILL.md 由字段拼装）。
    ///
    /// # Errors
    /// 名字非法 / 已存在 / frontmatter 字段缺失。
    pub fn create(
        &self,
        name: &str,
        description: &str,
        modules: &[String],
        license: Option<&str>,
        compatibility: Option<&str>,
        body: &str,
    ) -> Result<(), String> {
        if !is_valid_skill_name(name) {
            return Err(format!("invalid skill name '{name}' (lowercase/digits/hyphen)"));
        }
        let dir = self.root.join(name);
        if dir.exists() {
            return Err(format!("skill '{name}' already exists"));
        }
        fs::create_dir_all(&dir).map_err(|error| format!("cannot create skill dir: {error}"))?;
        let text = render_skill_md(name, description, modules, license, compatibility, body);
        fs::write(dir.join("SKILL.md"), text)
            .map_err(|error| format!("cannot write SKILL.md: {error}"))?;
        Ok(())
    }

    /// 删除 skill 目录。
    ///
    /// # Errors
    /// 名字非法或目录删除失败。
    pub fn delete(&self, name: &str) -> Result<(), String> {
        let dir = self
            .skill_dir(name)
            .ok_or_else(|| format!("invalid skill name '{name}'"))?;
        if !dir.exists() {
            return Err(format!("skill '{name}' does not exist"));
        }
        fs::remove_dir_all(&dir).map_err(|error| format!("cannot delete skill: {error}"))
    }

    /// 导入 zip（skill 根 = zip 内最浅的 SKILL.md 所在目录）。
    ///
    /// # Errors
    /// zip 无 SKILL.md、名字非法或落盘失败。
    pub fn import_zip(&self, bytes: &[u8]) -> Result<String, String> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|error| format!("invalid zip: {error}"))?;
        // 找最浅 SKILL.md（zip 目录前缀归一化）。
        let mut best: Option<(usize, String)> = None;
        for index in 0..archive.len() {
            let file = archive
                .by_index(index)
                .map_err(|error| format!("invalid zip entry: {error}"))?;
            let name = file.name().replace('\\', "/");
            let parts: Vec<&str> = name.split('/').filter(|part| !part.is_empty()).collect();
            if parts.last().is_some_and(|last| *last == "SKILL.md") {
                let depth = parts.len() - 1;
                if best.as_ref().is_none_or(|(best_depth, _)| depth < *best_depth) {
                    let root = if depth == 0 {
                        String::new()
                    } else {
                        format!("{}/", parts[..depth].join("/"))
                    };
                    best = Some((depth, root));
                }
            }
        }
        let Some((_, zip_root)) = best else {
            return Err("zip does not contain a SKILL.md".to_string());
        };
        // 抽取该子树；skill 名取 SKILL.md frontmatter 的 name:。
        let mut skill_name = String::new();
        let mut extracted: Vec<(String, Vec<u8>)> = Vec::new();
        for index in 0..archive.len() {
            let mut file = archive
                .by_index(index)
                .map_err(|error| format!("invalid zip entry: {error}"))?;
            let name = file.name().replace('\\', "/");
            let Some(rel) = name.strip_prefix(&zip_root) else {
                continue;
            };
            if rel.is_empty() || name.ends_with('/') {
                continue;
            }
            // 拒绝路径穿越与绝对路径。
            if rel.contains("..") || rel.starts_with('/') {
                return Err(format!("unsafe zip entry: {name}"));
            }
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes)
                .map_err(|error| format!("cannot read zip entry {name}: {error}"))?;
            if rel == "SKILL.md" {
                let text = String::from_utf8_lossy(&bytes).to_string();
                let (meta, _) = SkillMeta::parse(&text)
                    .ok_or_else(|| "SKILL.md frontmatter is invalid".to_string())?;
                if !is_valid_skill_name(&meta.name) {
                    return Err(format!("invalid skill name in SKILL.md: '{}'", meta.name));
                }
                skill_name = meta.name;
            }
            extracted.push((rel.to_string(), bytes));
        }
        if skill_name.is_empty() {
            return Err("SKILL.md frontmatter is missing".to_string());
        }
        let dir = self.root.join(&skill_name);
        if dir.exists() {
            return Err(format!("skill '{skill_name}' already exists"));
        }
        for (rel, bytes) in &extracted {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("cannot create skill subdir: {error}"))?;
            }
            fs::write(&path, bytes).map_err(|error| format!("cannot write {rel}: {error}"))?;
        }
        Ok(skill_name)
    }

    fn resolve_inner(&self, name: &str, relative: &str) -> Result<PathBuf, String> {
        let dir = self
            .skill_dir(name)
            .ok_or_else(|| format!("invalid skill name '{name}'"))?;
        let normalized = relative.replace('\\', "/");
        if normalized.split('/').any(|part| part == ".." || part.is_empty()) {
            return Err("invalid skill file path".to_string());
        }
        Ok(dir.join(normalized))
    }
}

/// 仓库内置 skill 的播种源（相对进程 cwd，与 `from_env` 的 `data/skills` 同口径）。
///
/// `data/` 被 `.gitignore` 忽略，全新 clone 上运行时根是空的；内置 skill 随
/// 仓库发在 `resources/skills/`，靠播种落到运行时根才能真正被 `list`/`load`
/// 看见。
#[must_use]
pub fn default_resources_root() -> PathBuf {
    PathBuf::from("resources").join("skills")
}

/// 把内置 skill 播种到运行时根。
///
/// **只增不覆盖**：目标 skill 目录已存在就整体跳过——用户经 UI/API 新建或改过的
/// skill 是权威，绝不被发货内容冲掉。因此重复调用幂等，可放在每次启动路径上。
///
/// 名字非法（非 lowercase/digits/hyphen）或没有 SKILL.md 的目录一律跳过：那说明
/// 不是 skill，硬播种只会制造一个 `list` 永远看不见的垃圾目录。
///
/// 返回本次实际播种的 skill 名（已存在被跳过的不会出现在结果里）。
#[must_use]
pub fn seed_skills(resources_root: &Path, runtime_root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(resources_root) else {
        return Vec::new();
    };
    let mut seeded = Vec::new();
    for entry in entries.flatten() {
        let source = entry.path();
        if !source.is_dir() {
            continue;
        }
        let Some(name) = source.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_valid_skill_name(name) || !source.join("SKILL.md").is_file() {
            continue;
        }
        let target = runtime_root.join(name);
        if target.exists() {
            continue;
        }
        if fs::create_dir_all(&runtime_root).is_err() {
            continue;
        }
        if copy_dir_recursive(&source, &target).is_ok() {
            seeded.push(name.to_string());
        }
    }
    seeded.sort();
    seeded
}

/// 递归复制目录内容（播种专用：目标刚建好，不存在需要合并的既有文件）。
fn copy_dir_recursive(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &destination)?;
        } else {
            fs::copy(entry.path(), &destination)?;
        }
    }
    Ok(())
}

fn collect_files(
    root: &Path,
    current: PathBuf,
    out: &mut Vec<String>,
    depth: usize,
) -> Result<(), String> {
    if depth > 8 {
        return Ok(());
    }
    let entries = fs::read_dir(&current)
        .map_err(|error| format!("cannot read skill dir: {error}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, path, out, depth + 1)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// 按字段拼装 SKILL.md 文本。
#[must_use]
pub fn render_skill_md(
    name: &str,
    description: &str,
    modules: &[String],
    license: Option<&str>,
    compatibility: Option<&str>,
    body: &str,
) -> String {
    let mut text = String::from("---\n");
    text.push_str(&format!("name: {name}\n"));
    text.push_str(&format!("description: {description}\n"));
    if let Some(license) = license {
        text.push_str(&format!("license: {license}\n"));
    }
    if let Some(compatibility) = compatibility {
        text.push_str(&format!("compatibility: {compatibility}\n"));
    }
    if !modules.is_empty() {
        text.push_str(&format!("modules: {}\n", modules.join(", ")));
    }
    text.push_str("---\n\n");
    text.push_str(body);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_manager() -> (SkillManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        (SkillManager::new(dir.path().to_path_buf()), dir)
    }

    #[test]
    fn frontmatter_parse_requires_name_and_description() {
        let (meta, body) =
            SkillMeta::parse("---\nname: api-recon\ndescription: 收集网站API接口。\nmodules: nuclei, httpx\n---\n\n# 手册正文\n")
                .expect("valid frontmatter");
        assert_eq!(meta.name, "api-recon");
        assert_eq!(meta.modules, ["nuclei", "httpx"]);
        assert!(body.starts_with("# 手册正文"));
        assert!(SkillMeta::parse("---\nname: only-name\n---\nbody").is_none());
    }

    #[test]
    fn create_load_delete_roundtrip() {
        let (manager, _dir) = temp_manager();
        manager
            .create("api-recon", "收集网站API接口", &["nuclei".into()], None, None, "# 手册\n步骤…")
            .expect("create");
        let skills = manager.list().expect("list");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "api-recon");
        let (meta, body) = manager.load("api-recon").expect("load");
        assert_eq!(meta.description, "收集网站API接口");
        assert!(body.contains("手册"));
        assert!(manager.file_tree("api-recon").expect("tree").contains(&"SKILL.md".to_string()));
        manager.write_file("api-recon", "scripts/probe.sh", "#!/bin/sh\necho hi")
            .expect("write");
        assert_eq!(
            manager.read_file("api-recon", "scripts/probe.sh").expect("read"),
            "#!/bin/sh\necho hi"
        );
        // 路径穿越拒绝。
        assert!(manager.read_file("api-recon", "../escape").is_err());
        manager.delete("api-recon").expect("delete");
        assert!(manager.list().expect("list").is_empty());
    }

    #[test]
    fn invalid_names_are_rejected() {
        assert!(is_valid_skill_name("api-recon"));
        assert!(is_valid_skill_name("recon2"));
        assert!(!is_valid_skill_name(""));
        assert!(!is_valid_skill_name("API"));
        assert!(!is_valid_skill_name("../evil"));
        assert!(!is_valid_skill_name("-lead"));
    }

    /// 回归：`data/` 被 gitignore，全新 clone 上运行时根是空的。播种是把
    /// `resources/skills/` 里的内置 skill 落到运行时根的唯一通路——没有它，
    /// `/skills` 恒空且 MCP 的 skill_list/skill_load 全部找不到东西。
    #[test]
    fn seed_skills_copies_builtin_skills_into_the_runtime_root() {
        let resources = tempfile::tempdir().expect("resources tempdir");
        let runtime = tempfile::tempdir().expect("runtime tempdir");
        write_skill(resources.path(), "api-recon", &["SKILL.md", "scripts/harvest.py"]);
        write_skill(resources.path(), "ctf-web", &["SKILL.md"]);
        // 非 skill 目录：没有 SKILL.md，硬播种只会制造 list 看不见的垃圾目录。
        std::fs::create_dir_all(resources.path().join("not-a-skill")).expect("dir");
        std::fs::write(resources.path().join("not-a-skill/readme.md"), b"x").expect("file");

        let seeded = seed_skills(resources.path(), runtime.path());

        assert_eq!(seeded, vec!["api-recon", "ctf-web"], "只播种合法 skill");
        assert!(runtime.path().join("api-recon/SKILL.md").is_file());
        assert!(
            runtime.path().join("api-recon/scripts/harvest.py").is_file(),
            "子目录必须递归复制——skill 常带 scripts/"
        );
        assert!(
            !runtime.path().join("not-a-skill").exists(),
            "没有 SKILL.md 的目录不是 skill"
        );
    }

    /// 只增不覆盖：用户经 UI/API 建过或改过的 skill 是权威，绝不被发货内容
    /// 冲掉。这也是重复启动必须幂等的原因。
    #[test]
    fn seed_skills_never_overwrites_user_owned_skills() {
        let resources = tempfile::tempdir().expect("resources tempdir");
        let runtime = tempfile::tempdir().expect("runtime tempdir");
        write_skill(resources.path(), "api-recon", &["SKILL.md"]);
        std::fs::create_dir_all(runtime.path().join("api-recon")).expect("dir");
        std::fs::write(
            runtime.path().join("api-recon/SKILL.md"),
            b"---\nname: api-recon\ndescription: user edited\n---\n",
        )
        .expect("user copy");

        let seeded = seed_skills(resources.path(), runtime.path());

        assert!(seeded.is_empty(), "已存在的 skill 必须整体跳过");
        let kept = std::fs::read_to_string(runtime.path().join("api-recon/SKILL.md"))
            .expect("read user copy");
        assert!(
            kept.contains("user edited"),
            "用户的改写必须原样保留，实际内容: {kept}"
        );
    }

    /// 播种源不存在（仓库被裁剪 / 路径配错）时必须安静返回，不能 panic。
    #[test]
    fn seed_skills_is_a_noop_without_a_resources_root() {
        let runtime = tempfile::tempdir().expect("runtime tempdir");
        let missing = runtime.path().join("does-not-exist");
        assert!(seed_skills(&missing, runtime.path()).is_empty());
    }

    /// 全新 clone 上 `data/skills` 压根不存在：`list` 要返回空列表而不是
    /// Err，否则 `/skills` 在播种跑之前就 500。
    #[test]
    fn list_returns_empty_when_the_runtime_root_is_absent() {
        let runtime = tempfile::tempdir().expect("runtime tempdir");
        let manager = SkillManager::new(runtime.path().join("no-such-dir"));
        assert!(
            manager.list().expect("absent root is not an error").is_empty(),
            "根目录不存在 = 暂无 skill，不是错误"
        );
    }

    /// 测试辅助：在给定根下造一个最小合法 skill。
    fn write_skill(root: &std::path::Path, name: &str, files: &[&str]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test skill\n---\n\n# body\n"),
        )
        .expect("SKILL.md");
        for relative in files.iter().filter(|relative| **relative != "SKILL.md") {
            let path = dir.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("subdir");
            std::fs::write(path, b"# stub\n").expect("file");
        }
    }
}
