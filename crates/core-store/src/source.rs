use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

/// 编译器会读取的全部源文件（相对工程根目录）。`build/` 永远不在其中。
pub const SOURCE_FILES: &[&str] = &[
    "project.yaml",
    "intent.yaml",
    "structure.yaml",
    "content.yaml",
    "narration.yaml",
    "voices.yaml",
    "overrides.yaml",
    "rules.yaml",
    "queue.yaml",
    "media/index.yaml",
    "locks/tts.lock.json",
];

/// 工程源文件的内存快照。编译只依赖它，不碰文件系统。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceSet {
    files: BTreeMap<String, String>,
}

impl SourceSet {
    pub fn read_dir(root: &Path) -> io::Result<SourceSet> {
        let mut files = BTreeMap::new();
        for name in SOURCE_FILES {
            match fs::read_to_string(root.join(name)) {
                Ok(text) => {
                    files.insert((*name).to_string(), text);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(SourceSet { files })
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.files.get(name).map(String::as_str)
    }

    pub fn insert(&mut self, name: &str, text: String) {
        self.files.insert(name.to_string(), text);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.files.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// 写入编译产物。只允许写 `build/` 下的文件。
pub fn write_build(root: &Path, files: &BTreeMap<String, Vec<u8>>) -> io::Result<()> {
    for (name, bytes) in files {
        if !name.starts_with("build/") || name.contains("..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("拒绝写入 build/ 之外的路径：{name}"),
            ));
        }
        let path = root.join(name);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, bytes)?;
    }
    Ok(())
}
