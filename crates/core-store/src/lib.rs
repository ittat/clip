//! 读写工程目录、校验引用、规范化输出。

pub mod canon;
mod load;
mod source;

pub use load::{load, LoadError, LoadErrors};
pub use source::{write_build, SourceSet, SOURCE_FILES};
