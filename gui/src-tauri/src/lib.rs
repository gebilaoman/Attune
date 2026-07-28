// Attune —— 英语听读学习工具后端(Tauri 2)。
//
// 命令分组:
// - 配置 / vault:get_config / save_config_command / pick_directory / open_vault / reveal_path
// - 文档库:list_vault / load_note / set_mastered / delete_note
// - 导入(AI 四步清理):import_text
// - 音频生成(edge-tts):generate_audio / load_audio_data
// - 即查:lookup_word
//
// 存储约定:
// - JSON 是唯一真身,跟用户选的 vault 文件夹走(Obsidian 式)。
// - 配置(provider/key/vault/tts)留在 app 数据目录(~/Library/Application Support/Attune)。
// - 一篇文档 = vault 内一个 .json;句级 mp3 放在该文档所在文件夹的 media/ 下,文件名 = 句 id。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod update;

use attune_core::{strip_code_fence, AIProvider, Note, Sentence};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

// ─────────────────────────── 配置 ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub api_keys: HashMap<String, String>,
    /// vault 根目录(绝对路径)。空串表示尚未选库。
    #[serde(default)]
    pub vault_path: String,
    /// TTS 配置(厂商/音色/语速/各厂凭证/音色列表),独立于模型配置。
    #[serde(default)]
    pub tts: TtsConfig,
}

impl Default for StoredConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            base_url: default_base_url(),
            model: default_model(),
            api_keys: HashMap::new(),
            vault_path: String::new(),
            tts: TtsConfig::default(),
        }
    }
}

// ─────────────────────────── TTS(嵌套) ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsConfig {
    /// edge / zhipu / doubao / aliyun
    #[serde(default = "default_tts_provider")]
    pub provider: String,
    #[serde(default = "default_tts_voice")]
    pub voice: String,
    #[serde(default = "default_tts_rate")]
    pub rate: String,
    /// 是否在朗读正文前先读说话人姓名(默认 true)。
    #[serde(default = "default_read_speaker")]
    pub read_speaker: bool,
    #[serde(default)]
    pub credentials: TtsCredentials,
    /// 各厂商可选音色:provider -> [[id, 显示名], ...]。用户可自行增删。
    #[serde(default)]
    pub voices: HashMap<String, Vec<[String; 2]>>,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            provider: default_tts_provider(),
            voice: default_tts_voice(),
            rate: default_tts_rate(),
            read_speaker: default_read_speaker(),
            credentials: TtsCredentials::default(),
            voices: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TtsCredentials {
    #[serde(default)]
    pub zhipu: ZhipuCred,
    #[serde(default)]
    pub doubao: DoubaoCred,
    #[serde(default)]
    pub aliyun: AliyunCred,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZhipuCred {
    #[serde(default)]
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoubaoCred {
    /// X-Api-Key(控制台 API Key,api-key- 开头)
    #[serde(default)]
    pub api_key: String,
    /// X-Api-Resource-Id,豆包语音合成大模型 2.0 = seed-tts-2.0
    #[serde(default = "default_doubao_resource")]
    pub resource_id: String,
}
impl Default for DoubaoCred {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            resource_id: default_doubao_resource(),
        }
    }
}
fn default_doubao_resource() -> String {
    "seed-tts-2.0".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliyunCred {
    #[serde(default)]
    pub app_key: String,
    #[serde(default)]
    pub access_key_id: String,
    #[serde(default)]
    pub access_key_secret: String,
    #[serde(default = "default_aliyun_region")]
    pub region: String,
}
impl Default for AliyunCred {
    fn default() -> Self {
        Self {
            app_key: String::new(),
            access_key_id: String::new(),
            access_key_secret: String::new(),
            region: default_aliyun_region(),
        }
    }
}

fn default_provider() -> String {
    "zhipu".to_string()
}
fn default_base_url() -> String {
    provider_default_base_url("zhipu").to_string()
}
fn default_model() -> String {
    "glm-5.2".to_string()
}
fn default_tts_voice() -> String {
    "en-US-AriaNeural".to_string()
}
fn default_tts_provider() -> String {
    "edge".to_string()
}
fn default_tts_rate() -> String {
    "-8%".to_string() // 默认略慢,适合学习者
}
fn default_read_speaker() -> bool {
    true
}
fn default_aliyun_region() -> String {
    "cn-shanghai".to_string()
}

fn provider_default_base_url(provider: &str) -> &'static str {
    match provider {
        "openai" => "https://api.openai.com/v1",
        "deepseek" => "https://api.deepseek.com",
        "openrouter" => "https://openrouter.ai/api/v1",
        _ => "https://open.bigmodel.cn/api/paas/v4",
    }
}

/// 首次使用时给各厂商 seed 一组默认音色(写入配置,用户随后可改)。
fn seed_voices(config: &mut StoredConfig) {
    if !config.tts.voices.is_empty() {
        return;
    }
    let mut v: HashMap<String, Vec<[String; 2]>> = HashMap::new();
    v.insert(
        "edge".to_string(),
        vec![
            ["en-US-AriaNeural".into(), "Aria(女,自然)".into()],
            ["en-US-JennyNeural".into(), "Jenny(女,亲切)".into()],
            ["en-US-EmmaNeural".into(), "Emma(女,沉稳)".into()],
            ["en-US-GuyNeural".into(), "Guy(男)".into()],
            ["en-US-AndrewNeural".into(), "Andrew(男,自然)".into()],
            ["en-GB-SoniaNeural".into(), "Sonia(英式女)".into()],
        ],
    );
    v.insert(
        "zhipu".to_string(),
        vec![["female".into(), "彤彤(女,默认)".into()]],
    );
    v.insert(
        "doubao".to_string(),
        vec![
            // seed-tts-2.0 要用 _bigtts 格式音色;BVxxx_streaming 是 1.0,会报 resource mismatch。
            ["en_female_dacey_uranus_bigtts".into(), "Dacey(美式女)".into()],
            ["en_female_stokie_uranus_bigtts".into(), "Stokie(美式女)".into()],
            ["en_male_tim_uranus_bigtts".into(), "Tim(美式男)".into()],
            ["zh_female_vv_uranus_bigtts".into(), "Vivi(中文女,可英)".into()],
        ],
    );
    v.insert(
        "aliyun".to_string(),
        vec![
            ["xiaoyun".into(), "小云(女)".into()],
            ["ailun".into(), "艾伦(男)".into()],
        ],
    );
    config.tts.voices = v;
}

// ─────────────────────────── 对前端的公开视图 ───────────────────────────

#[derive(Debug, Serialize)]
pub struct PublicTts {
    pub provider: String,
    pub voice: String,
    pub rate: String,
    pub read_speaker: bool,
    pub voices: HashMap<String, Vec<[String; 2]>>,
    pub has_zhipu_key: bool,
    pub doubao_resource_id: String,
    pub has_doubao_key: bool,
    pub aliyun_app_key: String,
    pub aliyun_access_key_id: String,
    pub aliyun_region: String,
    pub has_aliyun_secret: bool,
}

#[derive(Debug, Serialize)]
pub struct PublicConfig {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub has_api_key: bool,
    pub config_path: String,
    pub vault_path: String,
    pub tts: PublicTts,
}

// ─────────────────────────── 保存入参 ───────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct TtsCredInput {
    #[serde(default)]
    pub zhipu: Option<ZhipuCred>,
    #[serde(default)]
    pub doubao: Option<DoubaoCred>,
    #[serde(default)]
    pub aliyun: Option<AliyunCred>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TtsInput {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub rate: Option<String>,
    #[serde(default)]
    pub read_speaker: Option<bool>,
    #[serde(default)]
    pub credentials: Option<TtsCredInput>,
    #[serde(default)]
    pub voices: Option<HashMap<String, Vec<[String; 2]>>>,
}

#[derive(Debug, Deserialize)]
pub struct ConfigInput {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub vault_path: Option<String>,
    #[serde(default)]
    pub tts: Option<TtsInput>,
}

fn get_data_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()))
        .join("Attune")
}

fn get_config_path() -> PathBuf {
    get_data_dir().join("config.json")
}

fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建数据目录失败: {}", e))?;
    }
    let content =
        serde_json::to_string_pretty(value).map_err(|e| format!("序列化 JSON 失败: {}", e))?;
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("打开 JSON 文件失败: {}", e))?;
    file.write_all(content.as_bytes())
        .map_err(|e| format!("写入 JSON 失败: {}", e))?;
    #[cfg(unix)]
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .map_err(|e| format!("设置 JSON 权限失败: {}", e))?;
    Ok(())
}

fn load_config() -> StoredConfig {
    let path = get_config_path();
    if path.exists() {
        if let Ok(content) = fs::read_to_string(&path) {
            // 先当 JSON Value 处理:迁移旧的扁平 tts_* 字段 → 嵌套 tts
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&content) {
                migrate_tts(&mut v);
                if let Ok(mut config) = serde_json::from_value::<StoredConfig>(v) {
                    if config.base_url.trim().is_empty() {
                        config.base_url = provider_default_base_url(&config.provider).to_string();
                    }
                    if config.tts.provider.trim().is_empty() {
                        config.tts.provider = default_tts_provider();
                    }
                    if config.tts.voice.trim().is_empty() {
                        config.tts.voice = default_tts_voice();
                    }
                    if config.tts.rate.trim().is_empty() {
                        config.tts.rate = default_tts_rate();
                    }
                    let voices_was_empty = config.tts.voices.is_empty();
                    seed_voices(&mut config);
                    if voices_was_empty {
                        // 把 seed 的默认音色落盘,方便用户在 config.json 里直接增删
                        let _ = save_config(&config);
                    }
                    return config;
                }
            }
        }
    }
    let mut def = StoredConfig::default();
    seed_voices(&mut def);
    def
}

/// 配置迁移:① 旧扁平 tts_* → 嵌套 tts;② 嵌套 doubao 旧结构(appid/token/cluster)→ V3 的 api_key。
fn migrate_tts(v: &mut serde_json::Value) {
    let obj = match v.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    // ① 没有嵌套 tts:从旧扁平字段构建
    if !obj.contains_key("tts") {
        let get = |k: &str| obj.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let old_appid = get("tts_doubao_appid");
        let doubao_api_key = if old_appid.starts_with("api-key") || old_appid.starts_with("sk-") {
            old_appid
        } else {
            get("tts_doubao_token")
        };
        let tts = serde_json::json!({
            "provider": get("tts_provider"),
            "voice": get("tts_voice"),
            "rate": get("tts_rate"),
            "credentials": {
                "zhipu": { "api_key": get("tts_zhipu_api_key") },
                "doubao": { "api_key": doubao_api_key, "resource_id": "seed-tts-2.0" }
            }
        });
        obj.insert("tts".to_string(), tts);
    }
    // ② 嵌套 doubao 若还是旧 V1 结构(appid/token),挽救成 V3 api_key
    let d = obj
        .get_mut("tts")
        .and_then(|t| t.get_mut("credentials"))
        .and_then(|c| c.get_mut("doubao"))
        .and_then(|d| d.as_object_mut());
    if let Some(d) = d {
        if !d.contains_key("api_key") {
            let appid = d.get("appid").and_then(|x| x.as_str()).unwrap_or("");
            let token = d.get("token").and_then(|x| x.as_str()).unwrap_or("");
            let key = if appid.starts_with("api-key") || appid.starts_with("sk-") {
                appid.to_string()
            } else {
                token.to_string()
            };
            d.insert("api_key".to_string(), serde_json::json!(key));
            d.entry("resource_id").or_insert_with(|| serde_json::json!("seed-tts-2.0"));
            d.remove("appid");
            d.remove("token");
            d.remove("cluster");
        }
    }
}

fn save_config(config: &StoredConfig) -> Result<(), String> {
    let path = get_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {}", e))?;
    }
    write_private_json(&path, config)
}

fn build_provider(config: &StoredConfig) -> Result<AIProvider, String> {
    let api_key = config
        .api_keys
        .get(&config.provider)
        .filter(|key| !key.is_empty())
        .cloned()
        .ok_or_else(|| format!("尚未配置 {} 的 API Key", config.provider))?;
    Ok(AIProvider::new(api_key)
        .with_model(config.model.clone())
        .with_base_url(config.base_url.clone())
        // 智谱 GLM 是推理模型,交互式任务(校对/清理/查词)关掉思考,避免单次请求慢到几分钟。
        .with_thinking_disabled(config.provider.trim() == "zhipu"))
}

fn public_view(config: &StoredConfig) -> PublicConfig {
    let has_api_key = config
        .api_keys
        .get(&config.provider)
        .is_some_and(|key| !key.is_empty());
    PublicConfig {
        provider: config.provider.clone(),
        base_url: config.base_url.clone(),
        model: config.model.clone(),
        has_api_key,
        config_path: get_config_path().display().to_string(),
        vault_path: config.vault_path.clone(),
        tts: {
            let t = &config.tts;
            PublicTts {
                provider: t.provider.clone(),
                voice: t.voice.clone(),
                rate: t.rate.clone(),
                read_speaker: t.read_speaker,
                voices: t.voices.clone(),
                has_zhipu_key: !t.credentials.zhipu.api_key.is_empty(),
                doubao_resource_id: t.credentials.doubao.resource_id.clone(),
                has_doubao_key: !t.credentials.doubao.api_key.is_empty(),
                aliyun_app_key: t.credentials.aliyun.app_key.clone(),
                aliyun_access_key_id: t.credentials.aliyun.access_key_id.clone(),
                aliyun_region: t.credentials.aliyun.region.clone(),
                has_aliyun_secret: !t.credentials.aliyun.access_key_secret.is_empty(),
            }
        },
    }
}

// ─────────────────────────── vault / 路径 ───────────────────────────

fn vault_root(config: &StoredConfig) -> Result<PathBuf, String> {
    let path = config.vault_path.trim();
    if path.is_empty() {
        return Err("尚未选择库目录(vault)。请到设置里选一个文件夹作为你的英语库。".to_string());
    }
    let root = PathBuf::from(path);
    if !root.is_dir() {
        return Err(format!("库目录不存在或不是文件夹: {}", path));
    }
    Ok(root)
}

/// 文档一级目录 <vault>/docs:所有文档(及其子文件夹分类)都放这里,与 <vault>/media 分开、互不干扰。
/// 首次访问自动创建。
fn docs_root(config: &StoredConfig) -> Result<PathBuf, String> {
    let dir = vault_root(config)?.join("docs");
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| format!("创建文档目录失败: {}", e))?;
    }
    Ok(dir)
}

/// 把标题安全化成文件名(去掉路径分隔符等非法字符)。
fn sanitize_filename(name: &str) -> String {
    let trimmed = name.trim();
    let fallback = "未命名";
    let name = if trimmed.is_empty() { fallback } else { trimmed };
    let mut out = String::new();
    for ch in name.chars() {
        if matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    let out = out.trim_matches('.').trim();
    if out.is_empty() {
        fallback.to_string()
    } else {
        out.to_string()
    }
}

/// 计算文档 JSON 在 vault 内的相对路径:`<folder>/<safe_title>.json`,重名则追加短 id。
fn note_rel_path(folder: &str, title: &str, note_id: &str, config: &StoredConfig) -> PathBuf {
    let mut rel = PathBuf::new();
    if !folder.trim().is_empty() {
        rel.push(folder.trim());
    }
    rel.push(format!("{}.json", sanitize_filename(title)));
    if let Ok(root) = docs_root(config) {
        let abs = root.join(&rel);
        if abs.exists() {
            rel.set_file_name(format!("{}-{}.json", sanitize_filename(title), &note_id[..12.min(note_id.len())]));
        }
    }
    rel
}

/// 从文档相对路径取所在文件夹(去掉文件名)。用于把 note.folder 校正为磁盘实际位置:
/// 避免用户在 Finder 里手动移动文档后,改标题/重转时按 JSON 里记的旧 folder 把它挪回去。
fn folder_from_rel(rel: &str) -> String {
    let norm = rel.replace('\\', "/");
    match norm.rsplit_once('/') {
        Some((dir, _file)) => dir.to_string(),
        None => String::new(),
    }
}

/// 安全地把文档相对路径拼到文档根 <vault>/docs(禁止 `..` / 绝对路径,防穿越)。
fn resolve_in_vault(rel: &str, config: &StoredConfig) -> Result<PathBuf, String> {
    let root = docs_root(config)?;
    let mut full = root.clone();
    for seg in rel.split(['/', '\\']) {
        match seg {
            "" | "." => continue,
            ".." => return Err("非法路径".to_string()),
            other => full.push(other),
        }
    }
    Ok(full)
}

// ─────────────────────────── 文档 id / 句 id ───────────────────────────

fn gen_note_id() -> String {
    let ms = Utc::now().timestamp_millis() as u64;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("note_{:x}{:x}", ms, nanos & 0xffff)
}

/// 给文档里每句分配 id / audio 文件名(导入后调用一次)。
/// audio 是「相对该文档 media/ 目录」的路径:每篇独占一个子目录 `{note.id}/{序号}.mp3`,
/// 删除/移动/重转都能按目录整体操作。真实扩展名由 TTS 脚本按厂商写回(edge/豆包 mp3、智谱 wav)。
fn assign_sentence_ids(note: &mut Note) {
    let prefix = note.id.clone();
    let mut n = 1usize;
    for block in &mut note.blocks {
        match block {
            attune_core::Block::Paragraph { sentences } => {
                for s in sentences {
                    s.id = format!("{}_{}", prefix, n);
                    s.audio = format!("{}/{}.mp3", prefix, n);
                    n += 1;
                }
            }
            attune_core::Block::List { items } => {
                for item in items {
                    for s in &mut item.sentences {
                        s.id = format!("{}_{}", prefix, n);
                        s.audio = format!("{}/{}.mp3", prefix, n);
                        n += 1;
                    }
                }
            }
        }
    }
}

fn locate_sentence_mut<'a>(note: &'a mut Note, id: &str) -> Option<&'a mut Sentence> {
    note.sentences_mut().into_iter().find(|s| s.id == id)
}

// ─────────────────────────── 列表 ───────────────────────────

#[derive(Debug, Serialize)]
pub struct NoteRef {
    pub rel_path: String,
    pub id: String,
    pub title: String,
    pub folder: String,
    pub created_at: String,
    pub tags: Vec<String>,
    pub converted: bool,
    pub audio_ready: bool,
    pub total: usize,
    pub readable: usize,
    pub mastered: usize,
}

#[tauri::command]
fn list_vault() -> Result<Vec<NoteRef>, String> {
    let config = load_config();
    let root = docs_root(&config)?;
    let mut refs = Vec::new();
    walk_notes(&root, &root, &mut refs)?;
    // 最新创建的排前面
    refs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(refs)
}

/// 列出 vault 内所有文件夹(相对路径,含空文件夹,排除 media/隐藏)。
#[tauri::command]
fn list_folders() -> Result<Vec<String>, String> {
    let config = load_config();
    let root = docs_root(&config)?;
    let mut folders = Vec::new();
    walk_folders(&root, &root, &mut folders)?;
    folders.sort();
    Ok(folders)
}

fn walk_folders(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if let Ok(rel) = path.strip_prefix(root) {
                let r = rel.to_string_lossy().replace('\\', "/").to_string();
                if !r.is_empty() {
                    out.push(r);
                }
            }
            walk_folders(root, &path, out)?;
        }
    }
    Ok(())
}

/// 新建文件夹(支持嵌套,如 工作/新项目)。
#[tauri::command]
fn create_folder(name: String) -> Result<String, String> {
    let config = load_config();
    let root = docs_root(&config)?;
    let name = name.trim().trim_matches('/');
    if name.is_empty() {
        return Err("文件夹名不能为空".to_string());
    }
    let rel: String = name
        .split('/')
        .map(sanitize_filename)
        .collect::<Vec<_>>()
        .join("/");
    let abs = root.join(&rel);
    fs::create_dir_all(&abs).map_err(|e| format!("创建文件夹失败: {}", e))?;
    Ok(rel)
}

fn walk_notes(root: &Path, dir: &Path, out: &mut Vec<NoteRef>) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk_notes(root, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(note) = serde_json::from_str::<Note>(&content) {
                    let (total, readable) = note.sentence_counts();
                    let mastered = note
                        .collect_sentences()
                        .iter()
                        .filter(|s| s.mastered)
                        .count();
                    let rel = path
                        .strip_prefix(root)
                        .map(|p| p.to_string_lossy().replace('\\', "/").to_string())
                        .unwrap_or_default();
                    let folder = Path::new(&rel)
                        .parent()
                        .map(|p| p.to_string_lossy().replace('\\', "/").to_string())
                        .unwrap_or_default();
                    out.push(NoteRef {
                        rel_path: rel,
                        id: note.id,
                        title: note.title,
                        folder,
                        created_at: note.created_at,
                        tags: note.tags,
                        converted: note.converted || total > 0,
                        audio_ready: note.audio_ready,
                        total,
                        readable,
                        mastered,
                    });
                }
            }
        }
    }
    Ok(())
}

#[tauri::command]
fn load_note(rel_path: String) -> Result<Note, String> {
    let config = load_config();
    let abs = resolve_in_vault(&rel_path, &config)?;
    let content = fs::read_to_string(&abs)
        .map_err(|e| format!("读取文档失败: {}", e))?;
    serde_json::from_str::<Note>(&content).map_err(|e| format!("解析文档 JSON 失败: {}", e))
}

#[tauri::command]
fn set_mastered(rel_path: String, sentence_id: String, mastered: bool) -> Result<(), String> {
    let config = load_config();
    let abs = resolve_in_vault(&rel_path, &config)?;
    let content = fs::read_to_string(&abs).map_err(|e| format!("读取文档失败: {}", e))?;
    let mut note: Note =
        serde_json::from_str(&content).map_err(|e| format!("解析文档 JSON 失败: {}", e))?;
    match locate_sentence_mut(&mut note, &sentence_id) {
        Some(s) => s.mastered = mastered,
        None => return Err(format!("找不到句 {}", sentence_id)),
    }
    write_private_json(&abs, &note)
}

/// 仅更新「朗读说话人」开关(播放页快捷切换用)。
#[tauri::command]
fn set_tts_read_speaker(on: bool) -> Result<(), String> {
    let mut config = load_config();
    config.tts.read_speaker = on;
    save_config(&config)
}

/// 就地保存编辑后的句块(不经 AI)。比对哪些句的「英文/朗读」变了 → 删该句音频,按需清 audio_ready。
#[tauri::command]
fn save_blocks(rel_path: String, blocks: Vec<attune_core::Block>) -> Result<bool, String> {
    let config = load_config();
    let abs = resolve_in_vault(&rel_path, &config)?;
    let mut note: Note = serde_json::from_str(
        &fs::read_to_string(&abs).map_err(|e| format!("读取文档失败: {}", e))?,
    )
    .map_err(|e| format!("解析文档 JSON 失败: {}", e))?;

    // 旧句:英文+朗读 (音频只跟这俩走;改中文/说话人不影响音频)
    let old: HashMap<String, (String, bool)> = note
        .collect_sentences()
        .iter()
        .map(|s| (s.id.clone(), (s.en.clone(), s.read_aloud)))
        .collect();

    note.blocks = blocks;

    // 删掉「英文或朗读变了」的句的音频(避免音频念旧词);按 vault 根 media + note.id/序号 定位。
    let mut audio_changed = false;
    if let Ok(media_root) = vault_media_root(&config) {
        for s in note.collect_sentences() {
            let changed = old
                .get(&s.id)
                .map(|(o_en, o_ra)| *o_en != s.en || *o_ra != s.read_aloud)
                .unwrap_or(false);
            if changed {
                if let Some((note_id, seq)) = s.id.rsplit_once('_') {
                    remove_sentence_audio(&media_root, note_id, seq);
                }
                audio_changed = true;
            }
        }
    }
    if audio_changed {
        note.audio_ready = false;
    }
    write_private_json(&abs, &note)?;
    Ok(audio_changed)
}

#[tauri::command]
fn delete_note(rel_path: String) -> Result<(), String> {
    let config = load_config();
    let abs = resolve_in_vault(&rel_path, &config)?;
    // 先删这篇的音频子目录(<vault>/media/{note.id}/),避免留下孤儿音频。
    if let Ok(content) = fs::read_to_string(&abs) {
        if let Ok(note) = serde_json::from_str::<Note>(&content) {
            if let Ok(media_root) = vault_media_root(&config) {
                remove_note_media(&media_root, &note.id);
            }
        }
    }
    // 再移除文档 JSON
    trash_note(&abs);
    Ok(())
}

/// 删除一个文档文件夹(<vault>/docs/<rel>)及其内所有文档;连带清掉这些文档在 <vault>/media 下的音频。
#[tauri::command]
fn delete_folder(rel_path: String) -> Result<(), String> {
    let config = load_config();
    let abs = resolve_in_vault(&rel_path, &config)?;
    if rel_path.trim().is_empty() {
        return Err("不能删除根目录".to_string());
    }
    if !abs.is_dir() {
        return Err("文件夹不存在".to_string());
    }
    // media 按 note.id 存、与文档文件夹分离,需要显式清掉这个文件夹里每篇文档的音频。
    if let Ok(media_root) = vault_media_root(&config) {
        purge_media_under(&abs, &media_root);
    }
    fs::remove_dir_all(&abs).map_err(|e| format!("删除文件夹失败: {}", e))?;
    Ok(())
}

/// 递归:删掉 dir 内所有文档(*.json)对应的音频目录。
fn purge_media_under(dir: &Path, media_root: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            purge_media_under(&p, media_root);
        } else if p.extension().and_then(|x| x.to_str()) == Some("json") {
            if let Ok(content) = fs::read_to_string(&p) {
                if let Ok(note) = serde_json::from_str::<Note>(&content) {
                    remove_note_media(media_root, &note.id);
                }
            }
        }
    }
}

/// vault 根的统一音频目录 <vault>/media。音频按 note.id 索引,和文档所在子文件夹解耦:
/// 文档树与 media 树互不干扰,重组/移动文档不会让音频失联,也就不用重新生成。
fn vault_media_root(config: &StoredConfig) -> Result<PathBuf, String> {
    Ok(vault_root(config)?.join("media"))
}

/// 删除某篇文档的整个音频子目录 <vault>/media/{note_id}/(删除/重转/强制重生成共用)。
fn remove_note_media(media_root: &Path, note_id: &str) {
    if note_id.is_empty() {
        return;
    }
    let _ = fs::remove_dir_all(media_root.join(note_id));
}

fn trash_note(path: &Path) {
    // 自用工具:删错了从废纸篓找回。直接系统删除即可,避免引 trash 依赖的体积。
    let _ = fs::remove_file(path);
}

// ─────────────────────────── 导入(AI 四步清理) ───────────────────────────

#[derive(Debug, Deserialize)]
struct ImportPayload {
    #[serde(default)]
    title: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    blocks: Vec<attune_core::Block>,
}

fn import_prompt(raw: &str) -> String {
    format!(
        r#"你是英语听读材料的整理助手。我会给你一段【任意来源】的原始文本(Slack 聊天 / 会议纪要 / 英文博客 / 文档等)。请只做四件事,输出严格的 JSON:

1. 滤噪音:去掉界面残留(导航、按钮文字如 "翻译""发送""Reply")、时间戳、重复的图片/文件名、跨频道引用标记、输入框占位符等明显非内容的东西。
2. 保结构:按原文换行还原段落 / 列表。若某句有说话人,把说话人填进该句的 speaker 字段(没有就空串)。不要自作主张加小标题、不要合并段落。
3. 切句 + 标注不朗读元素:切成完整句子;链接 / 文件名 / 代码命令 / 截图说明 等替换成只显示不朗读的占位符,如 "See [link]" → 把 read_aloud 设为 false(正文仍保留占位符供阅读)。长句(从句多)不要再往下切子句。
4. 逐句翻译:每句配 zh(中文),中文不朗读、只显示。

硬性要求:
- 不绑定来源,不要做 Slack 专属的假设(私信/引用有用就并入正文,没用就丢)。
- speaker 要朗读(帮用户回忆场景),没有就空。
- 只输出 JSON,不要任何解释、不要 markdown 代码块。

输出格式:
{{
  "title": "简短中文标题",
  "tags": ["标签1"],
  "blocks": [
    {{ "type": "paragraph", "sentences": [
        {{ "speaker": "Dan Shao", "en": "Sounds good. Please send the doc.", "zh": "好的,请把文档发一下。", "read_aloud": true }}
    ]}},
    {{ "type": "list", "items": [
        {{ "sentences": [ {{ "speaker": "", "en": "See [link] for details.", "zh": "详见[链接]。", "read_aloud": false }} ] }}
    ]}}
  ]
}}

下面是需要整理的原始文本:

-----
{raw}
-----"#
    )
}

/// 调 AI 把原文清理成 blocks(四步:滤噪音/保结构/切句标注/翻译)。
async fn clean_via_ai(raw: &str, provider: &AIProvider) -> Result<ImportPayload, String> {
    let prompt = import_prompt(raw);
    let resp = provider.ask_raw(&prompt).await?;
    let cleaned = strip_code_fence(&resp);
    serde_json::from_str::<ImportPayload>(cleaned).map_err(|e| {
        format!("解析 AI 返回的 JSON 失败: {}\n\n原始返回(已去代码块):\n{}", e, cleaned)
    })
}

/// 单块字符上限(避免单次请求过大/超时:长文分块处理)。
const CLEAN_CHUNK_LIMIT: usize = 3000;

fn flush_chunk(cur: &mut String, chunks: &mut Vec<String>) {
    let t = cur.trim().to_string();
    if !t.is_empty() {
        chunks.push(t);
    }
    cur.clear();
}

/// 把长原文切成 ~CLEAN_CHUNK_LIMIT 字符的块:先按段落累积切,任何仍超限的块(单段过长)按字符硬切。
fn split_into_chunks(raw: &str) -> Vec<String> {
    let limit = CLEAN_CHUNK_LIMIT;
    let text = raw.trim();
    let paragraphs: Vec<&str> = text
        .split("\n\n")
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    for p in paragraphs {
        if !cur.is_empty() && cur.chars().count() + p.chars().count() > limit {
            flush_chunk(&mut cur, &mut chunks);
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(p);
    }
    flush_chunk(&mut cur, &mut chunks);
    if chunks.is_empty() {
        chunks.push(text.to_string());
    }
    enforce_limit(chunks, limit)
}

/// 把仍超过 limit 字符的块按字符硬切成多块。
fn enforce_limit(chunks: Vec<String>, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for c in chunks {
        if c.chars().count() <= limit {
            out.push(c);
            continue;
        }
        let chars: Vec<char> = c.chars().collect();
        let mut start = 0;
        while start < chars.len() {
            let end = (start + limit).min(chars.len());
            out.push(chars[start..end].iter().collect::<String>().trim().to_string());
            start = end;
        }
    }
    out
}

/// 长文分块清理 → 合并(title/tags 取首块,blocks 按原顺序拼接)。
/// 多块【限并发】处理(而非串行):墙钟时间从「各块求和」降到「最慢那块」,长文转化快数倍。
/// buffered 保序 —— 输出顺序与输入块一致,文档结构不乱。
async fn clean_text_chunked(
    raw: &str,
    provider: &AIProvider,
) -> Result<(String, Vec<String>, Vec<attune_core::Block>), String> {
    use futures::stream::{self, StreamExt, TryStreamExt};
    // 同时最多跑几块 AI:兼顾提速与厂商限流。
    const CONCURRENCY: usize = 4;

    let chunks = split_into_chunks(raw);
    if chunks.is_empty() {
        return Ok((String::new(), Vec::new(), Vec::new()));
    }
    // 先把各块的 future 收进 Vec(具体类型),再交给 stream::iter —— 避免把借用闭包直接
    // 传给 buffered 触发 HRTB 的「FnOnce is not general enough」编译错误。
    let futs: Vec<_> = chunks.iter().map(|ch| clean_via_ai(ch, provider)).collect();
    let payloads: Vec<ImportPayload> = stream::iter(futs)
        .buffered(CONCURRENCY)
        .try_collect()
        .await?;

    let mut title = String::new();
    let mut tags: Vec<String> = Vec::new();
    let mut blocks: Vec<attune_core::Block> = Vec::new();
    for (i, payload) in payloads.into_iter().enumerate() {
        if i == 0 {
            title = payload.title;
            tags = payload.tags;
        }
        blocks.extend(payload.blocks);
    }
    Ok((title, tags, blocks))
}

fn write_note_to_vault(note: &Note, config: &StoredConfig) -> Result<PathBuf, String> {
    let rel = note_rel_path(&note.folder, &note.title, &note.id, config);
    let abs = resolve_in_vault(&rel.to_string_lossy().replace('\\', "/"), config)?;
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建文档目录失败: {}", e))?;
    }
    write_private_json(&abs, note)?;
    Ok(abs)
}

#[tauri::command]
async fn import_text(
    raw: String,
    title: String,
    folder: String,
    source: Option<String>,
) -> Result<Note, String> {
    let raw = raw.trim().to_string();
    if raw.is_empty() {
        return Err("内容为空".to_string());
    }
    let config = load_config();
    let _root = vault_root(&config)?; // 必须先选库
    let provider = build_provider(&config)?;
    let (ai_title, ai_tags, ai_blocks) = clean_text_chunked(&raw, &provider).await?;

    let mut note = Note {
        id: gen_note_id(),
        title: if title.trim().is_empty() {
            ai_title
        } else {
            title.trim().to_string()
        },
        folder: folder.trim().to_string(),
        tags: ai_tags,
        created_at: Utc::now().format("%Y-%m-%d").to_string(),
        source,
        raw,
        converted: true,
        blocks: ai_blocks,
        audio_ready: false,
    };
    if note.title.trim().is_empty() {
        note.title = "未命名".to_string();
    }
    assign_sentence_ids(&mut note);
    write_note_to_vault(&note, &config)?;
    Ok(note)
}

/// 新建草稿:只有原文(可空),未转化。用户随后可编辑原文 → AI 转化。
#[tauri::command]
async fn create_draft(title: String, folder: String, raw: String) -> Result<Note, String> {
    let config = load_config();
    let _root = vault_root(&config)?;
    let note = Note {
        id: gen_note_id(),
        title: if title.trim().is_empty() {
            "新建文档".to_string()
        } else {
            title.trim().to_string()
        },
        folder: folder.trim().to_string(),
        tags: vec![],
        created_at: Utc::now().format("%Y-%m-%d").to_string(),
        source: None,
        raw,
        converted: false,
        blocks: vec![],
        audio_ready: false,
    };
    write_note_to_vault(&note, &config)?;
    Ok(note)
}

/// 保存编辑后的原文(可选改标题,改名会移动文件)。返回(可能新的) rel_path。
#[tauri::command]
fn save_raw(rel_path: String, raw: String, new_title: Option<String>) -> Result<String, String> {
    let config = load_config();
    let abs = resolve_in_vault(&rel_path, &config)?;
    let mut note: Note = serde_json::from_str(
        &fs::read_to_string(&abs).map_err(|e| format!("读取文档失败: {}", e))?,
    )
    .map_err(|e| format!("解析文档 JSON 失败: {}", e))?;
    note.folder = folder_from_rel(&rel_path); // 以磁盘实际位置为准,防手动移动后回弹
    note.raw = raw;
    let title_changed = new_title
        .as_deref()
        .map(|t| !t.trim().is_empty() && t.trim() != note.title)
        .unwrap_or(false);
    if title_changed {
        note.title = new_title.unwrap().trim().to_string();
    }
    // 若标题变了,写到新路径并删旧文件;否则原地写
    if title_changed {
        let new_abs = write_note_to_vault(&note, &config)?;
        let _ = fs::remove_file(&abs);
        Ok(new_abs
            .strip_prefix(docs_root(&config)?)
            .map(|p| p.to_string_lossy().replace('\\', "/").to_string())
            .unwrap_or_default())
    } else {
        write_private_json(&abs, &note)?;
        Ok(rel_path)
    }
}

/// AI 转化原文 → 句块(可反复:改完原文重新转化)。清掉旧音频(句 id 会变)。
#[tauri::command]
async fn convert_note(rel_path: String) -> Result<Note, String> {
    let config = load_config();
    let provider = build_provider(&config)?;
    let abs = resolve_in_vault(&rel_path, &config)?;
    let mut note: Note = serde_json::from_str(
        &fs::read_to_string(&abs).map_err(|e| format!("读取文档失败: {}", e))?,
    )
    .map_err(|e| format!("解析文档 JSON 失败: {}", e))?;
    note.folder = folder_from_rel(&rel_path); // 以磁盘实际位置为准,防手动移动后回弹
    let raw = note.raw.trim().to_string();
    if raw.is_empty() {
        return Err("原文为空，请先编辑原文再转化".to_string());
    }
    let (ai_title, ai_tags, ai_blocks) = clean_text_chunked(&raw, &provider).await?;
    note.blocks = ai_blocks;
    if note.tags.is_empty() {
        note.tags = ai_tags;
    }
    if note.title == "新建文档" && !ai_title.trim().is_empty() {
        note.title = ai_title;
    }
    note.converted = true;
    note.audio_ready = false;
    assign_sentence_ids(&mut note);
    // 句序号会随重新切分变化,直接清掉本篇整个音频子目录 <vault>/media/{note.id}/,下次生成从头来。
    if let Ok(media_root) = vault_media_root(&config) {
        remove_note_media(&media_root, &note.id);
    }
    // 写盘(标题若被 AI 改了,write_note_to_vault 按新标题算路径;旧文件删掉)
    let new_abs = write_note_to_vault(&note, &config)?;
    if new_abs != abs {
        let _ = fs::remove_file(&abs);
    }
    Ok(note)
}

/// 给 blocks 里的句从 start_n 起顺序发 id 与音频路径(用于「末尾追加」,只动这些新块,不碰其余句)。
fn assign_ids_from(blocks: &mut [attune_core::Block], prefix: &str, start_n: usize) {
    let mut n = start_n;
    for block in blocks {
        match block {
            attune_core::Block::Paragraph { sentences } => {
                for s in sentences {
                    s.id = format!("{}_{}", prefix, n);
                    s.audio = format!("{}/{}.mp3", prefix, n);
                    n += 1;
                }
            }
            attune_core::Block::List { items } => {
                for item in items {
                    for s in &mut item.sentences {
                        s.id = format!("{}_{}", prefix, n);
                        s.audio = format!("{}/{}.mp3", prefix, n);
                        n += 1;
                    }
                }
            }
        }
    }
}

/// AI 增量追加:只对新贴的原文跑清理→切句→翻译,结果接到现有 blocks 末尾。
/// 关键:新句从「现有最大序号 +1」发号,**不调用全局 assign_sentence_ids**,既有句 id / 音频不变。
/// 原文也接到 note.raw 末尾,保证「回原文重转」仍能复现整篇。
#[tauri::command]
async fn append_via_ai(rel_path: String, raw: String) -> Result<Note, String> {
    let raw = raw.trim().to_string();
    if raw.is_empty() {
        return Err("追加内容为空".to_string());
    }
    let config = load_config();
    let provider = build_provider(&config)?;
    let abs = resolve_in_vault(&rel_path, &config)?;
    let mut note: Note = serde_json::from_str(
        &fs::read_to_string(&abs).map_err(|e| format!("读取文档失败: {}", e))?,
    )
    .map_err(|e| format!("解析文档 JSON 失败: {}", e))?;
    note.folder = folder_from_rel(&rel_path); // 以磁盘实际位置为准

    // 只清理这段新文本 → 新 blocks(title/tags 忽略:追加不改文档标题/标签)。
    let (_ai_title, _ai_tags, mut new_blocks) = clean_text_chunked(&raw, &provider).await?;

    // 现有最大句序号:解析每句 id 尾部数字取 max;空文档从 0 起。
    let mut max_seq = 0usize;
    for s in note.collect_sentences() {
        if let Some((_, seq)) = s.id.rsplit_once('_') {
            if let Ok(n) = seq.parse::<usize>() {
                if n > max_seq {
                    max_seq = n;
                }
            }
        }
    }
    assign_ids_from(&mut new_blocks, &note.id, max_seq + 1);

    note.blocks.extend(new_blocks);
    // 原文接上(整篇重转可复现);首段前不留空。
    if note.raw.trim().is_empty() {
        note.raw = raw;
    } else {
        note.raw = format!("{}\n\n{}", note.raw.trim_end(), raw);
    }
    note.converted = true; // 已有句块,归为已转化
    write_private_json(&abs, &note)?;
    Ok(note)
}

// ─────────────────────────── 单句 AI 优化(校对转写错误) ───────────────────────────

#[derive(Debug, Serialize)]
pub struct SentenceOpt {
    pub en: String,
    pub zh: String,
    pub changed: bool,
}

fn optimize_prompt(en: &str, prev: &str, next: &str, hint: &str) -> String {
    // 用户备注:明确的修正线索(机器猜不出的专有名词/口误),必须优先采纳。
    let hint_block = if hint.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n\n用户补充提示(这是人工给出的明确线索,请务必据此修正,优先级高于你自己的判断):\n{}\n",
            hint.trim()
        )
    };
    format!(
        r#"你在校对一句【由语音转写而来】的英文。语音转写常见错误:专有名词/技术词被错拆或错拼(如 "Web View" 应为 "WebView"、"nest js" 应为 "Nest.js"、"type script" 应为 "TypeScript"、"loader G s" 可能是 "loader.js")、同音词错字、大小写错误、漏词或多词、把一个词拆成两个或把两个拼成一个、口头语导致的重复/断续。

订正明显的转写错误,让句子通顺可读;不要凭空改写原意或过度润色。若本来就没问题,原样返回。中文翻译要基于【订正后】的英文,通顺准确。{hint_block}

下面的上下文仅供判断歧义参考,【不要】把它们并进目标句:
上一句: {prev}
下一句: {next}

需要校对的句子:
{en}

输出严格 JSON,不要任何解释、不要 markdown 代码块:
{{"en": "订正后的英文", "zh": "对应的中文翻译"}}"#
    )
}

/// 对单句做 AI 校对(修转写错误),返回建议的 en/zh —— 不落盘,交前端预览确认。
/// hint:用户手动补充的修正提示(可空),用于机器猜不出的专有名词/口误。
#[tauri::command]
async fn optimize_sentence(
    rel_path: String,
    sentence_id: String,
    hint: Option<String>,
) -> Result<SentenceOpt, String> {
    let config = load_config();
    let abs = resolve_in_vault(&rel_path, &config)?;
    let note: Note = serde_json::from_str(
        &fs::read_to_string(&abs).map_err(|e| format!("读取文档失败: {}", e))?,
    )
    .map_err(|e| format!("解析文档 JSON 失败: {}", e))?;

    let sentences = note.collect_sentences();
    let idx = sentences
        .iter()
        .position(|s| s.id == sentence_id)
        .ok_or("找不到该句")?;
    let cur_en = sentences[idx].en.clone();
    let cur_zh = sentences[idx].zh.clone();
    let prev = idx.checked_sub(1).map(|i| sentences[i].en.as_str()).unwrap_or("");
    let next = sentences.get(idx + 1).map(|s| s.en.as_str()).unwrap_or("");

    let hint = hint.unwrap_or_default();
    let provider = build_provider(&config)?;
    let resp = provider
        .ask_raw(&optimize_prompt(&cur_en, prev, next, &hint))
        .await?;
    let cleaned = strip_code_fence(&resp);

    #[derive(Deserialize)]
    struct P {
        #[serde(default)]
        en: String,
        #[serde(default)]
        zh: String,
    }
    let p: P = serde_json::from_str(cleaned)
        .map_err(|e| format!("解析 AI 返回失败: {}\n原始返回:\n{}", e, cleaned))?;

    let new_en = if p.en.trim().is_empty() { cur_en.clone() } else { p.en.trim().to_string() };
    let new_zh = if p.zh.trim().is_empty() { cur_zh } else { p.zh.trim().to_string() };
    // 给了提示就一定展示结果(用户明确要求重优化);否则以英文是否变化为准。
    let changed = new_en != cur_en || !hint.trim().is_empty();
    Ok(SentenceOpt { en: new_en, zh: new_zh, changed })
}

/// 采纳优化:改写该句 en/zh 落盘,并作废该句在所有音色下的缓存(下次播放按新文本实时合成)。
#[tauri::command]
fn apply_sentence_edit(
    rel_path: String,
    sentence_id: String,
    en: String,
    zh: String,
) -> Result<(), String> {
    let config = load_config();
    let abs = resolve_in_vault(&rel_path, &config)?;
    let mut note: Note = serde_json::from_str(
        &fs::read_to_string(&abs).map_err(|e| format!("读取文档失败: {}", e))?,
    )
    .map_err(|e| format!("解析文档 JSON 失败: {}", e))?;

    {
        let s = locate_sentence_mut(&mut note, &sentence_id).ok_or("找不到该句")?;
        s.en = en.trim().to_string();
        s.zh = zh.trim().to_string();
    }
    write_private_json(&abs, &note)?;

    // 该句文本变了 → 各音色的旧音频作废
    if let Some((note_id, seq)) = sentence_id.rsplit_once('_') {
        if let Ok(media_root) = vault_media_root(&config) {
            remove_sentence_audio(&media_root, note_id, seq);
        }
    }
    Ok(())
}

/// 删除某句在所有音色子目录下的缓存音频(<vault>/media/{note_id}/*/{seq}.{mp3,wav})。
fn remove_sentence_audio(media_root: &Path, note_id: &str, seq: &str) {
    let base = media_root.join(note_id);
    if let Ok(entries) = fs::read_dir(&base) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                for ext in ["mp3", "wav"] {
                    let _ = fs::remove_file(e.path().join(format!("{}.{}", seq, ext)));
                }
            }
        }
    }
}

// ─────────────────────────── 音频生成(edge-tts) ───────────────────────────

#[derive(Debug, Serialize)]
pub struct AudioGenResult {
    pub generated: usize,
    pub skipped: usize,
    pub missing: usize,
    pub note_rel: String,
    pub log: String,
}

/// 预缓存进度:done=已处理句数,total=需音频的句总数。通过事件 `precache-progress` 推给前端。
#[derive(Debug, Clone, Serialize)]
pub struct PrecacheProgress {
    pub done: usize,
    pub total: usize,
}

/// 预处理脚本位置:repo 根的 scripts/tts_generate.py(相对 src-tauri 往上两级)。
fn tts_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/tts_generate.py")
}

#[tauri::command]
async fn generate_audio(
    app: tauri::AppHandle,
    rel_path: String,
    force: Option<bool>,
    voice: Option<String>,
    read_speaker: Option<bool>,
) -> Result<AudioGenResult, String> {
    use tauri::Emitter;
    let config = load_config();
    let abs = resolve_in_vault(&rel_path, &config)?;
    let script = tts_script_path();
    if !script.exists() {
        return Err(format!("找不到 TTS 脚本: {}", script.display()));
    }

    // 要预缓存的音色 / 读名字设定:前端传的(阅读器当前状态)优先,否则 config 默认。
    let voice = voice
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| config.tts.voice.clone());
    let read_speaker = read_speaker.unwrap_or(config.tts.read_speaker);

    // force=true:先删掉本文档整个音频子目录 media/{note.id}/(所有音色),强制重生成。
    // 用于「朗读姓名」开关切换 —— 正文变了,各音色旧缓存都作废。
    if force.unwrap_or(false) {
        if let Ok(content) = fs::read_to_string(&abs) {
            if let Ok(note) = serde_json::from_str::<Note>(&content) {
                if let Ok(media_root) = vault_media_root(&config) {
                    remove_note_media(&media_root, &note.id);
                }
            }
        }
    }

    // 流式跑脚本:边跑边读 stdout,解析 `@PROGRESS done total` 转发前端(precache-progress),
    // 其余行汇入日志。这样预缓存能实时显示「x/y 句」。
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;
    let mut child = std::process::Command::new("python3")
        .arg(script)
        .arg("--note")
        .arg(&abs)
        .arg("--config")
        .arg(get_config_path())
        .arg("--voice")
        .arg(&voice)
        .arg("--read-speaker")
        .arg(if read_speaker { "1" } else { "0" })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 python3 失败: {}(确认系统已装 edge-tts:pip install edge-tts)", e))?;

    let mut stdout_log = String::new();
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if let Some(rest) = line.strip_prefix("@PROGRESS ") {
                let mut it = rest.split_whitespace();
                if let (Some(a), Some(b)) = (it.next(), it.next()) {
                    if let (Ok(done), Ok(total)) = (a.parse::<usize>(), b.parse::<usize>()) {
                        let _ = app.emit("precache-progress", PrecacheProgress { done, total });
                    }
                }
                continue; // 进度行不进日志
            }
            stdout_log.push_str(&line);
            stdout_log.push('\n');
        }
    }
    let _ = child.wait();
    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut stderr);
    }
    let log = format!("{}\n{}", stdout_log, stderr);

    // 不再「一句失败就整篇报错」:脚本本身逐句容错、幂等续跑。
    // 以「磁盘上实际存在的音频」为准统计:成功的落盘保留,缺的下次点「生成」自动补齐。
    let media_dir = vault_media_root(&config).ok();
    let mut generated = 0usize; // 需朗读且音频已就绪
    let mut skipped = 0usize; // 不朗读,无需音频
    let mut missing = 0usize; // 需朗读但音频仍缺失(可重试补齐)
    if let Ok(content) = fs::read_to_string(&abs) {
        if let Ok(mut note) = serde_json::from_str::<Note>(&content) {
            for s in note.collect_sentences() {
                if !s.read_aloud {
                    skipped += 1;
                    continue;
                }
                let ok = media_dir
                    .as_ref()
                    .map(|m| m.join(&s.audio))
                    .and_then(|p| fs::metadata(p).ok())
                    .map(|meta| meta.len() > 0)
                    .unwrap_or(false);
                if ok {
                    generated += 1;
                } else {
                    missing += 1;
                }
            }
            // 全部朗读句都就绪才算 audio_ready(允许 0 朗读句的文档直接就绪)
            note.audio_ready = missing == 0;
            let _ = write_private_json(&abs, &note);
        }
    }

    // 完全没产出且确有缺失 → 当作硬错误,把脚本日志抛给前端(常见:未装 edge-tts / 凭证错误)
    if generated == 0 && missing > 0 {
        return Err(format!("TTS 生成失败:\n{}", log));
    }

    Ok(AudioGenResult {
        generated,
        skipped,
        missing,
        note_rel: rel_path,
        log,
    })
}

/// 把音色 id 清洗成文件系统安全的目录名(须与 Python voice_key 规则一致)。
/// 保留 ASCII 字母数字与 - _,其余换成 _;空则回退 "default"。
fn sanitize_voice(voice: &str) -> String {
    let key: String = voice
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if key.is_empty() {
        "default".to_string()
    } else {
        key
    }
}

/// 缓存子目录名(须与 Python cache_folder 一致):读名字=纯音色名(默认,现有文件即此);
/// 不读名字=音色名 + "-nospk"。让「读名字」成为热开关的一个缓存维度。
fn cache_folder(voice: &str, read_speaker: bool) -> String {
    let vkey = sanitize_voice(voice);
    if read_speaker {
        vkey
    } else {
        format!("{}-nospk", vkey)
    }
}

/// 读一个音频文件成 data URL(按扩展名给 mime;WKWebView 下用 data URL 规避自定义 scheme 的坑)。
fn audio_file_to_data_url(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("读取音频失败({}): {}", path.display(), e))?;
    use base64_encode::encode;
    let mime = if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("wav"))
        .unwrap_or(false)
    {
        "audio/wav"
    } else {
        "audio/mpeg"
    };
    Ok(format!("data:{};base64,{}", mime, encode(&bytes)))
}

/// 播放某句:按「当前音色」取缓存,命中直接读、未命中实时合成后再读,返回 data URL。
/// 缓存路径 media/{note.id}/{音色}/{序号}.ext —— 每个音色各自缓存,听读中途可随时切音色。
#[tauri::command]
async fn play_sentence(
    rel_path: String,
    sentence_id: String,
    voice: Option<String>,
    read_speaker: Option<bool>,
) -> Result<String, String> {
    let config = load_config();
    let abs = resolve_in_vault(&rel_path, &config)?;
    let media = vault_media_root(&config)?;
    // sentence_id 形如 {note.id}_{序号};note.id 自身可含下划线,取最后一段作序号。
    let (note_id, seq) = sentence_id
        .rsplit_once('_')
        .ok_or_else(|| format!("非法句 id: {}", sentence_id))?;
    // 音色 / 读名字:优先用前端传的(热切换),否则用 config 默认。
    let voice = voice
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| config.tts.voice.clone());
    let read_speaker = read_speaker.unwrap_or(config.tts.read_speaker);
    let folder = cache_folder(&voice, read_speaker);
    let ext = if config.tts.provider.trim() == "zhipu" {
        "wav"
    } else {
        "mp3"
    };
    let cache = media
        .join(note_id)
        .join(&folder)
        .join(format!("{}.{}", seq, ext));

    // 命中:直接读缓存(离线、秒回放)
    if fs::metadata(&cache).map(|m| m.len() > 0).unwrap_or(false) {
        return audio_file_to_data_url(&cache);
    }

    // 未命中:实时合成(脚本 --only 覆盖当前音色 / 读名字设定),再读
    let script = tts_script_path();
    if !script.exists() {
        return Err(format!("找不到 TTS 脚本: {}", script.display()));
    }
    let output = std::process::Command::new("python3")
        .arg(&script)
        .arg("--note")
        .arg(&abs)
        .arg("--config")
        .arg(get_config_path())
        .arg("--only")
        .arg(&sentence_id)
        .arg("--voice")
        .arg(&voice)
        .arg("--read-speaker")
        .arg(if read_speaker { "1" } else { "0" })
        .output()
        .map_err(|e| format!("启动 python3 失败: {}", e))?;
    let raw = String::from_utf8_lossy(&output.stdout);
    let path = raw
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .last()
        .unwrap_or_default()
        .to_string();
    if !output.status.success() || path.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(format!("合成失败:\n{}", stderr));
    }
    audio_file_to_data_url(Path::new(&path))
}

/// 试听:用已保存的 config(厂商/音色/凭证)合成一句样本,返回 data URL 供前端播放。
#[tauri::command]
async fn test_tts() -> Result<String, String> {
    let script = tts_script_path();
    if !script.exists() {
        return Err(format!("找不到 TTS 脚本: {}", script.display()));
    }
    let output = std::process::Command::new("python3")
        .arg(&script)
        .arg("--test")
        .arg("--config")
        .arg(get_config_path())
        .output()
        .map_err(|e| format!("启动 python3 失败: {}", e))?;
    // 脚本会先打印一行厂商日志(如 [edge-tts] ...),路径在最后一行 —— 取最后一个非空行。
    let raw_stdout = String::from_utf8_lossy(&output.stdout);
    let path = raw_stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .last()
        .unwrap_or_default()
        .to_string();
    if !output.status.success() || path.is_empty() {
        // 把 stdout(含 [厂商] voice=… 上下文)+ stderr(真实错误)一起回传,前端红字显示
        let mut msg = String::new();
        let so = raw_stdout.trim().to_string();
        let se = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !so.is_empty() {
            msg.push_str(&so);
        }
        if !se.is_empty() {
            if !msg.is_empty() {
                msg.push('\n');
            }
            msg.push_str(&se);
        }
        return Err(format!("试听失败:\n{}", msg));
    }
    let bytes = fs::read(&path).map_err(|e| format!("读取试听音频失败({}): {}", path, e))?;
    use base64_encode::encode;
    let mime = if path.to_ascii_lowercase().ends_with(".wav") {
        "audio/wav"
    } else {
        "audio/mpeg"
    };
    Ok(format!("data:{};base64,{}", mime, encode(&bytes)))
}

/// 把一个句级 mp3 读成 data URL 供前端 <audio> 播放。
/// (用 data URL 而非自定义协议,规避 WKWebView 自定义 scheme / 中文路径解码的坑。)
#[tauri::command]
fn load_audio_data(rel_path: String, audio: String) -> Result<String, String> {
    let config = load_config();
    let _ = resolve_in_vault(&rel_path, &config)?; // 校验路径合法(防目录穿越)
    let media_dir = vault_media_root(&config)?;
    audio_file_to_data_url(&media_dir.join(&audio))
}

// ─────────────────────────── 模型列表(通用)───────────────────────────

#[derive(Debug, Serialize)]
pub struct FetchedModel {
    pub id: String,
    #[serde(default)]
    pub owned_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<FetchedModelEntry>,
}

#[derive(Debug, Deserialize)]
struct FetchedModelEntry {
    id: String,
    #[serde(default)]
    owned_by: Option<String>,
}

/// 按 OpenAI 兼容约定拼 /models 候选 URL(有的带 /v1 有的不带)。
fn build_models_url_candidates(base_url: &str) -> Result<Vec<String>, String> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("Base URL 不能为空".to_string());
    }
    let last_segment = base.rsplit('/').next().unwrap_or_default();
    let is_version_segment = last_segment
        .strip_prefix('v')
        .is_some_and(|value| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()));
    let mut candidates = if is_version_segment {
        vec![format!("{base}/models")]
    } else {
        vec![format!("{base}/v1/models"), format!("{base}/models")]
    };
    candidates.dedup();
    Ok(candidates)
}

#[tauri::command]
async fn fetch_models(
    provider: String,
    base_url: String,
    api_key: Option<String>,
) -> Result<Vec<FetchedModel>, String> {
    let stored = load_config();
    let key = api_key
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .or_else(|| stored.api_keys.get(&provider).cloned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("尚未配置 {} API Key", provider))?;
    let candidates = build_models_url_candidates(&base_url)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("创建请求客户端失败: {}", e))?;
    let mut errors = Vec::new();
    for url in candidates {
        let response = match client.get(&url).bearer_auth(&key).send().await {
            Ok(response) => response,
            Err(error) => {
                errors.push(format!("{}: {}", url, error));
                continue;
            }
        };
        let status = response.status();
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Err(
                    "当前 Provider 的 API Key 无效或没有模型列表权限，请重新填写并保存".to_string(),
                );
            }
            errors.push(format!("{}: HTTP {}", url, status));
            continue;
        }
        let response: ModelsResponse = response
            .json()
            .await
            .map_err(|e| format!("模型列表响应解析失败: {}", e))?;
        let mut models: Vec<FetchedModel> = response
            .data
            .into_iter()
            .map(|model| FetchedModel {
                id: model.id,
                owned_by: model.owned_by,
            })
            .collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models.dedup_by(|a, b| a.id == b.id);
        return Ok(models);
    }
    Err(format!("无法获取模型列表：{}", errors.join("；")))
}

#[tauri::command]
fn open_data_directory() -> Result<(), String> {
    let dir = get_data_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败: {}", e))?;
    open_in_finder(&dir)
}

// ─────────────────────────── 即查 ───────────────────────────

#[tauri::command]
async fn lookup_word(word: String, context: String) -> Result<String, String> {
    let config = load_config();
    let provider = build_provider(&config)?;
    let prompt = format!(
        "你在帮一个英语学习者「即查单词」。请结合下面的【句子上下文】,用中文解释「{word}」在这句话里的含义。\n\
         要求:\n\
         - 第一行:这个词最核心的中文释义。\n\
         - 第二行起:它在这句话里的具体意思(解决一词多义 / 行话,例如 cut 在 \"cut a 1.0.1 release\" 里是「发布/打版本」而不是「切」),并简要点一下常见用法或搭配。\n\
         - 简短,3~6 行以内,不要长篇大论。\n\n\
         句子上下文:{context}"
    );
    provider.ask_raw(&prompt).await
}

// ─────────────────────────── 配置 / 文件夹命令 ───────────────────────────

#[tauri::command]
async fn pick_directory(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let folder = app
        .dialog()
        .file()
        .set_title("选择库文件夹")
        .blocking_pick_folder();
    Ok(folder
        .and_then(|path| path.into_path().ok())
        .map(|path| path.to_string_lossy().trim_end_matches('/').to_string()))
}

#[tauri::command]
fn pick_subfolder() -> Result<(), String> {
    // 占位:前端用 pick_directory 选库根;子文件夹通过文本输入 / 新建。
    Ok(())
}

#[tauri::command]
fn get_config() -> PublicConfig {
    public_view(&load_config())
}

#[tauri::command]
fn save_config_command(input: ConfigInput) -> Result<PublicConfig, String> {
    if input.base_url.trim().is_empty() {
        return Err("Base URL 不能为空".to_string());
    }
    if input.model.trim().is_empty() {
        return Err("请先选择或填写模型".to_string());
    }
    let mut stored = load_config();
    if let Some(api_key) = input
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
    {
        stored
            .api_keys
            .insert(input.provider.clone(), api_key.trim().to_string());
    }
    stored.provider = input.provider.clone();
    stored.base_url = input.base_url.trim().trim_end_matches('/').to_string();
    stored.model = input.model.trim().to_string();
    if let Some(vp) = input.vault_path {
        stored.vault_path = vp.trim().trim_end_matches('/').to_string();
    }
    // TTS:合并嵌套 tts 入参。密码类字段(智谱 api_key / 豆包 token / 阿里 secret)留空=不改。
    if let Some(t) = input.tts {
        if let Some(p) = t.provider {
            let p = p.trim().to_string();
            if matches!(p.as_str(), "edge" | "zhipu" | "doubao" | "aliyun") {
                stored.tts.provider = p;
            }
        }
        if let Some(v) = t.voice {
            stored.tts.voice = v.trim().to_string();
        }
        if let Some(r) = t.rate {
            stored.tts.rate = r.trim().to_string();
        }
        if let Some(rs) = t.read_speaker {
            stored.tts.read_speaker = rs;
        }
        if let Some(voices) = t.voices {
            stored.tts.voices = voices; // 前端算好整体增删后全量替换
        }
        if let Some(c) = t.credentials {
            if let Some(z) = c.zhipu {
                let k = z.api_key.trim().to_string();
                if !k.is_empty() {
                    stored.tts.credentials.zhipu.api_key = k;
                }
            }
            if let Some(d) = c.doubao {
                let rid = d.resource_id.trim().to_string();
                stored.tts.credentials.doubao.resource_id =
                    if rid.is_empty() { default_doubao_resource() } else { rid };
                let key = d.api_key.trim().to_string();
                if !key.is_empty() {
                    stored.tts.credentials.doubao.api_key = key;
                }
            }
            if let Some(a) = c.aliyun {
                stored.tts.credentials.aliyun.app_key = a.app_key.trim().to_string();
                stored.tts.credentials.aliyun.access_key_id = a.access_key_id.trim().to_string();
                let region = a.region.trim().to_string();
                stored.tts.credentials.aliyun.region = if region.is_empty() {
                    default_aliyun_region()
                } else {
                    region
                };
                let sec = a.access_key_secret.trim().to_string();
                if !sec.is_empty() {
                    stored.tts.credentials.aliyun.access_key_secret = sec;
                }
            }
        }
    }
    save_config(&stored)?;
    Ok(public_view(&stored))
}

#[tauri::command]
fn open_vault() -> Result<(), String> {
    let config = load_config();
    let root = vault_root(&config)?;
    open_in_finder(&root)
}

#[tauri::command]
fn reveal_path(rel_path: String) -> Result<(), String> {
    let config = load_config();
    let abs = resolve_in_vault(&rel_path, &config)?;
    reveal_in_finder(&abs)
}

#[tauri::command]
fn start_window_drag(window: tauri::Window) -> Result<(), String> {
    window
        .start_dragging()
        .map_err(|e| format!("启动窗口拖拽失败: {}", e))
}

fn open_in_finder(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("打开失败: {}", e))?;
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err("仅支持 macOS".to_string())
    }
}

fn reveal_in_finder(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map_err(|e| format!("打开失败: {}", e))?;
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err("仅支持 macOS".to_string())
    }
}

// ─────────────────────────── base64(零依赖小实现) ───────────────────────────

mod base64_encode {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0];
            let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };
            out.push(TABLE[(b0 >> 2) as usize] as char);
            out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(TABLE[(b2 & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }
}

// ─────────────────────────── 启动 ───────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config_command,
            pick_directory,
            pick_subfolder,
            open_vault,
            reveal_path,
            list_vault,
            list_folders,
            create_folder,
            delete_folder,
            load_note,
            set_mastered,
            set_tts_read_speaker,
            save_blocks,
            delete_note,
            import_text,
            create_draft,
            save_raw,
            convert_note,
            append_via_ai,
            optimize_sentence,
            apply_sentence_edit,
            generate_audio,
            play_sentence,
            test_tts,
            load_audio_data,
            fetch_models,
            open_data_directory,
            lookup_word,
            start_window_drag,
            update::get_app_version,
            update::get_system_info
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_long_text_by_paragraphs() {
        // 短文本:单块
        assert_eq!(split_into_chunks("Hello world.").len(), 1);
        // 多段长文:按 ~CLEAN_CHUNK_LIMIT 切成多块,且每块不超限
        let para: String = "这是一段较长的正文内容。".repeat(150); // ~1800 字
        let raw = format!("{}\n\n{}\n\n{}", para, para, para);
        let chunks = split_into_chunks(&raw);
        assert!(chunks.len() >= 2, "长文应切成多块,实际 {}", chunks.len());
        for c in &chunks {
            assert!(c.chars().count() <= CLEAN_CHUNK_LIMIT * 2, "单块过大");
        }
        // 单段超长:按行切,不丢内容
        let big = "一句话。".repeat(1000); // 单段 ~5000 字
        let one_big = split_into_chunks(&big);
        assert!(one_big.len() >= 2, "单段超长应再切");
    }

    /// 模拟 AI 四步清理的返回(严格按 prompt 约定的 JSON 形状),
    /// 验证:能反序列化 → 构建 Note → 分配句 id/audio → 落盘 JSON 往返一致。
    #[test]
    fn ai_payload_round_trips_into_note() {
        let ai_json = r#"{
            "title": "SDK 集成对齐",
            "tags": ["会议", "SDK"],
            "blocks": [
                { "type": "paragraph", "sentences": [
                    { "speaker": "Dan Shao", "en": "Sounds good. Please send the doc and code.", "zh": "好的,请把文档和代码发一下。", "read_aloud": true }
                ]},
                { "type": "list", "items": [
                    { "sentences": [ { "speaker": "", "en": "See [link] for details.", "zh": "详见[链接]。", "read_aloud": false } ] },
                    { "sentences": [ { "speaker": "", "en": "SDK performance improvements.", "zh": "SDK 性能改进。", "read_aloud": true } ] }
                ]}
            ]
        }"#;

        let payload: ImportPayload = serde_json::from_str(ai_json).expect("解析 AI JSON");

        let mut note = Note {
            id: "note_abc123".to_string(),
            title: payload.title.clone(),
            folder: "工作/SDK集成".to_string(),
            tags: payload.tags.clone(),
            created_at: "2026-07-25".to_string(),
            source: None,
            raw: String::new(),
            converted: true,
            blocks: payload.blocks,
            audio_ready: false,
        };
        assign_sentence_ids(&mut note);

        let sentences = note.collect_sentences();
        assert_eq!(sentences.len(), 3, "应有 3 句");

        // id / audio 已分配:audio = 相对 media/ 的每篇子目录路径 {note.id}/{序号}.mp3
        assert_eq!(sentences[0].id, "note_abc123_1");
        assert_eq!(sentences[0].audio, "note_abc123/1.mp3");
        assert_eq!(sentences[0].speaker, "Dan Shao");
        assert!(sentences[0].read_aloud);

        // 占位句保留 read_aloud:false
        assert!(!sentences[1].read_aloud);
        assert_eq!(sentences[2].id, "note_abc123_3");

        let (total, readable) = note.sentence_counts();
        assert_eq!((total, readable), (3, 2));

        // JSON 往返:序列化 → 反序列化应一致(mastered 默认 false)
        let s = serde_json::to_string(&note).unwrap();
        let back: Note = serde_json::from_str(&s).unwrap();
        assert_eq!(back.collect_sentences().len(), 3);
        assert!(!back.collect_sentences()[0].mastered);

        // locate + 标记 mastered
        let mut note2 = back;
        locate_sentence_mut(&mut note2, "note_abc123_2")
            .unwrap()
            .mastered = true;
        assert!(note2.collect_sentences()[1].mastered);
    }

    /// 相对路径防穿越:`..` / 绝对路径必须被拒。
    #[test]
    fn resolve_in_vault_rejects_traversal() {
        let mut config = StoredConfig::default();
        config.vault_path = "/tmp/_attune_vault_test".to_string();
        std::fs::create_dir_all(&config.vault_path).ok();
        assert!(resolve_in_vault("工作/SDK集成/x.json", &config).is_ok());
        assert!(resolve_in_vault("../etc/passwd", &config).is_err());
        assert!(resolve_in_vault("a/../../b", &config).is_err());
    }

    #[test]
    fn strips_code_fence_helper() {
        assert_eq!(strip_code_fence("```json\n{}\n```"), "{}");
        assert_eq!(strip_code_fence("{\"a\":1}"), "{\"a\":1}");
    }
}
