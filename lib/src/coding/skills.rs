use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use roc_desk_common::fsops::FileOps;
use roc_desk_core::error::AppError;

const SKILLS_DIR: &str = ".rock_desk/skills";

/// Metadata for one skill (no body -- the body is loaded on demand via the
/// `skill` tool), used to render the "available skills: name -- description"
/// line in the system prompt so the model can decide which one to load. Also
/// the type handed straight to the frontend by the `skill_list` command (the
/// Skills management dialog's "view" uses it to render the list); field
/// names stay snake_case to match the rest of this crate's Tauri command
/// return types.
#[derive(Debug, Clone, Serialize)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    /// Skill directory path relative to the workspace root, e.g.
    /// `.rock_desk/skills/demo`.
    pub dir: String,
}

/// Minimal YAML frontmatter parsing: splits the `---`-delimited header into
/// `key: value` lines, no nested structures/multi-line values -- `SKILL.md`
/// frontmatter only ever uses the two scalar fields `name`/`description`, not
/// worth pulling in a full yaml crate for. Returns `(fields, body)`; when no
/// well-formed `---`-wrapped header is found, the whole content is treated as
/// the body and the field map is empty.
pub fn parse_frontmatter(text: &str) -> (HashMap<String, String>, &str) {
    let Some(rest) = text.strip_prefix("---") else {
        return (HashMap::new(), text);
    };
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let Some(end) = rest.find("\n---") else {
        return (HashMap::new(), text);
    };
    let header = &rest[..end];
    let body = rest[end + 4..]
        .strip_prefix('\n')
        .unwrap_or(&rest[end + 4..]);

    let mut fields = HashMap::new();
    for line in header.lines() {
        if let Some((key, value)) = line.split_once(':') {
            fields.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }
    (fields, body)
}

/// Scans `<workspace_root>/.rock_desk/skills/*/SKILL.md`, parsing each one's
/// frontmatter. A single skill directory failing to parse (no SKILL.md,
/// missing frontmatter fields, etc.) is skipped without affecting discovery
/// of the others -- like its caller (`coding_start`), this is a "nice to
/// have" capability that shouldn't block session startup over one
/// malformed skill directory.
pub async fn discover_skills(file_ops: &dyn FileOps, root: &str) -> Vec<SkillMeta> {
    let skills_root = format!("{}/{SKILLS_DIR}", root.trim_end_matches(['/', '\\']));
    let Ok(entries) = file_ops.list_dir(&skills_root).await else {
        return Vec::new();
    };

    let mut skills = Vec::new();
    for entry in entries.into_iter().filter(|e| e.is_dir) {
        let skill_md_path = format!("{}/SKILL.md", entry.path);
        let Ok(content) = file_ops.read_file(&skill_md_path).await else {
            continue;
        };
        let (fields, _body) = parse_frontmatter(&content.text);
        let name = fields
            .get("name")
            .cloned()
            .unwrap_or_else(|| entry.name.clone());
        let description = fields.get("description").cloned().unwrap_or_default();
        skills.push(SkillMeta {
            name,
            description,
            dir: entry.path.clone(),
        });
    }
    skills
}

/// Executes the `skill` tool: finds the skill directory by name, reads
/// `SKILL.md`'s body (the part after frontmatter) and returns it to the
/// model.
pub async fn load_skill_body(
    file_ops: &dyn FileOps,
    skills: &[SkillMeta],
    name: &str,
) -> Result<String, AppError> {
    let meta = skills
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| AppError::NotFound(format!("未找到技能：{name}")))?;
    let content = file_ops
        .read_file(&format!("{}/SKILL.md", meta.dir))
        .await?;
    let (_fields, body) = parse_frontmatter(&content.text);
    Ok(body.to_string())
}

/// Used by the `skill_import` command to tell "is this an archive or an
/// already-extracted folder" -- extension only, no file-header sniffing
/// (importing is already a file the user actively picked, not worth extra
/// validation against a renamed fake archive).
pub fn is_archive_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".zip") || lower.ends_with(".tar.gz") || lower.ends_with(".tgz")
}

/// Extracts a Skills archive (.zip/.tar.gz/.tgz) into `dest_dir` (caller is
/// responsible for preparing a fresh empty directory, usually a temp dir,
/// deleted once done). RAR is deliberately unsupported: there's no solid
/// pure-Rust RAR extraction crate, supporting it would mean shelling out to
/// a system-installed WinRAR/7-Zip or bundling an unrar executable into the
/// portable build, and Skills packages are never distributed as RAR
/// (GitHub/Claude Skills marketplaces only ship zip/tar.gz) -- not worth a
/// new external dependency for.
///
/// Returns the directory inside the archive that actually contains
/// `SKILL.md` -- many archives (especially GitHub's "Download ZIP") wrap
/// everything in one extra same-named folder (`my-skill-main/SKILL.md`,
/// not `SKILL.md` directly at the archive root), so this probes: if the
/// extracted root directly has `SKILL.md`, use the root; otherwise look for
/// exactly one subdirectory under the root that itself contains
/// `SKILL.md`; if neither is found, or there's more than one such
/// subdirectory (ambiguous which one to import), error out rather than
/// guessing.
pub fn extract_skill_archive(archive_path: &Path, dest_dir: &Path) -> Result<PathBuf, AppError> {
    let lower = archive_path.to_string_lossy().to_lowercase();
    if lower.ends_with(".zip") {
        extract_zip(archive_path, dest_dir)?;
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        extract_tar_gz(archive_path, dest_dir)?;
    } else {
        return Err(AppError::Internal(format!(
            "不支持的压缩包格式（只支持 .zip/.tar.gz/.tgz）：{}",
            archive_path.display()
        )));
    }
    find_skill_root(dest_dir)
}

fn extract_zip(archive_path: &Path, dest_dir: &Path) -> Result<(), AppError> {
    let file = std::fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::Internal(format!("zip 压缩包解析失败：{e}")))?;
    archive
        .extract(dest_dir)
        .map_err(|e| AppError::Internal(format!("zip 压缩包解压失败：{e}")))?;
    Ok(())
}

fn extract_tar_gz(archive_path: &Path, dest_dir: &Path) -> Result<(), AppError> {
    let file = std::fs::File::open(archive_path)?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    archive
        .unpack(dest_dir)
        .map_err(|e| AppError::Internal(format!("tar.gz 压缩包解压失败：{e}")))?;
    Ok(())
}

fn find_skill_root(dest_dir: &Path) -> Result<PathBuf, AppError> {
    if dest_dir.join("SKILL.md").is_file() {
        return Ok(dest_dir.to_path_buf());
    }
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(dest_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && path.join("SKILL.md").is_file() {
            candidates.push(path);
        }
    }
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => Err(AppError::Internal(
            "压缩包里没有找到 SKILL.md，不是一个合法的技能包".into(),
        )),
        _ => Err(AppError::Internal(
            "压缩包里有多个含 SKILL.md 的目录，无法确定要导入哪一个".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_body() {
        let text = "---\nname: demo\ndescription: 一个示例技能\n---\n\n这里是正文\n第二行\n";
        let (fields, body) = parse_frontmatter(text);
        assert_eq!(fields.get("name").map(String::as_str), Some("demo"));
        assert_eq!(
            fields.get("description").map(String::as_str),
            Some("一个示例技能")
        );
        assert!(body.contains("这里是正文"));
    }

    #[test]
    fn missing_frontmatter_returns_whole_text_as_body() {
        let text = "没有 frontmatter 的普通文本";
        let (fields, body) = parse_frontmatter(text);
        assert!(fields.is_empty());
        assert_eq!(body, text);
    }

    #[test]
    fn recognizes_archive_extensions() {
        assert!(is_archive_path("demo.zip"));
        assert!(is_archive_path("DEMO.ZIP"));
        assert!(is_archive_path("demo.tar.gz"));
        assert!(is_archive_path("demo.tgz"));
        assert!(!is_archive_path("demo"));
        assert!(!is_archive_path("demo.rar"));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("roc_desk-skills-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_skill_root_at_top_level() {
        let dir = temp_dir("root-level");
        std::fs::write(dir.join("SKILL.md"), "---\nname: demo\n---\nbody").unwrap();
        let found = find_skill_root(&dir).unwrap();
        assert_eq!(found, dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_skill_root_one_level_nested() {
        let dir = temp_dir("nested");
        let inner = dir.join("my-skill-main");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("SKILL.md"), "---\nname: demo\n---\nbody").unwrap();
        let found = find_skill_root(&dir).unwrap();
        assert_eq!(found, inner);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_when_no_skill_md_found() {
        let dir = temp_dir("empty");
        std::fs::write(dir.join("readme.txt"), "nothing here").unwrap();
        assert!(find_skill_root(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
