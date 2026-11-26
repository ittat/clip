//! 读写工程目录、校验引用、规范化输出。

pub mod canon;
mod load;
mod source;

pub use load::{load, LoadError, LoadErrors};
pub use source::{
    is_frozen_file, is_source_file, remove_source, write_build, write_source, SourceSet,
    SOLVER_LOCK, SOURCE_FILES,
};
