//! 分层剪辑器的核心类型。纯数据：无 IO、无浮点、无随机数。

pub mod cost;
pub mod id;
pub mod project;
pub mod tick;

pub use cost::Cost;
pub use id::*;
pub use project::*;
pub use tick::*;
