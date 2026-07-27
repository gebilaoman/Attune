//! attune-core:英语听读工具的核心逻辑(平台无关)。
//!
//! - `note`:文档 / 块 / 句 数据模型(听读真身)
//! - `ai_provider`:OpenAI 兼容的 chat 提供者,清理 / 翻译 / 查词统一走 `ask_raw`

pub mod ai_provider;
pub mod note;

pub use ai_provider::{strip_code_fence, AIProvider};
pub use note::{Block, ListItem, Note, Sentence};
