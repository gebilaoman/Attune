//! 文档(Note)数据模型 —— 听读工具的"真身"。
//!
//! 层级结构(通用,任何文本都成立):
//! ```text
//! 文档 / note   = 一次粘贴的东西(博客 / 会议 / Slack 聊天)
//!   └ 块        = 段落 / 列表项(文本天然的换行结构)
//!      └ 句     = 最小单元,一句 = 一个 mp3
//! ```
//!
//! 同一套 `Sentence` 结构同时用于:
//! 1. AI 导入输出(AI 只填 speaker/en/zh/read_aloud,id/audio/mastered 走默认值);
//! 2. 持久化的文档 JSON(全字段)。
//! 因此 `id` / `audio` / `mastered` 都带 `#[serde(default)]`,缺省也能解析。

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

/// 一篇文档 = 用户一次粘贴的东西。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    /// 文档 id,句 id 的前缀,例如 `note_a1b2c3`。
    pub id: String,
    pub title: String,
    /// vault 内的相对文件夹路径,如 `工作/SDK集成`。根目录为空串。
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 创建日期(YYYY-MM-DD)。
    pub created_at: String,
    /// 可选:文本来源备注(Slack / 博客 / 会议纪要 …)。
    #[serde(default)]
    pub source: Option<String>,
    /// 原始文本(可反复编辑筛检),AI 转化的输入。草稿阶段只有这个。
    #[serde(default)]
    pub raw: String,
    /// 是否已 AI 转化成句块(blocks 非空即视为已转化)。
    #[serde(default)]
    pub converted: bool,
    #[serde(default)]
    pub blocks: Vec<Block>,
    /// 句级 mp3 是否已生成完毕。
    #[serde(default)]
    pub audio_ready: bool,
}

/// 块 = 段落 / 列表。`type` 字段区分,内部 tagged enum。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Block {
    /// 段落:一组连续句子。
    Paragraph {
        #[serde(default)]
        sentences: Vec<Sentence>,
    },
    /// 列表:若干列表项,每项又含若干句。
    List {
        #[serde(default)]
        items: Vec<ListItem>,
    },
}

/// 列表项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    #[serde(default)]
    pub sentences: Vec<Sentence>,
}

/// 句 = 最小单元,一句对应一个 mp3。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sentence {
    /// 句 id,同时是音频文件名主干,如 `note_a1b2c3_003`。
    #[serde(default)]
    pub id: String,
    /// 可选说话人前缀(要朗读,帮用户回忆场景)。
    #[serde(default)]
    pub speaker: String,
    /// 英文(要朗读的正文)。
    #[serde(default)]
    pub en: String,
    /// 中文翻译(只显示不朗读)。
    #[serde(default)]
    pub zh: String,
    /// 音频文件名 = 句 id + `.mp3`。
    #[serde(default)]
    pub audio: String,
    /// 是否朗读。占位符(`[链接]` `[截图]`)为 false:显示但不生成音频。
    #[serde(default = "default_true")]
    pub read_aloud: bool,
    /// 是否已掌握(「懂了」)。精听/复习档默认跳过已掌握句。
    #[serde(default)]
    pub mastered: bool,
}

impl Note {
    /// 遍历文档里**所有**句(段落句 + 列表项句),按阅读顺序。
    pub fn sentences_mut(&mut self) -> Vec<&mut Sentence> {
        let mut out = Vec::new();
        for block in &mut self.blocks {
            match block {
                Block::Paragraph { sentences } => {
                    for s in sentences {
                        out.push(s);
                    }
                }
                Block::List { items } => {
                    for item in items {
                        for s in &mut item.sentences {
                            out.push(s);
                        }
                    }
                }
            }
        }
        out
    }

    /// 只读地收集所有句(阅读顺序)。
    pub fn collect_sentences(&self) -> Vec<&Sentence> {
        let mut out = Vec::new();
        for block in &self.blocks {
            match block {
                Block::Paragraph { sentences } => {
                    for s in sentences {
                        out.push(s);
                    }
                }
                Block::List { items } => {
                    for item in items {
                        for s in &item.sentences {
                            out.push(s);
                        }
                    }
                }
            }
        }
        out
    }

    /// 统计:总句数 / 可朗读句数。
    pub fn sentence_counts(&self) -> (usize, usize) {
        let all = self.collect_sentences();
        let readable = all.iter().filter(|s| s.read_aloud).count();
        (all.len(), readable)
    }
}
