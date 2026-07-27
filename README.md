# Attune · 英语听读学习工具

一个**个人自用**的英语听读工具。核心引擎只做一件事:

> 一段文本进来 → 清理干净 → 切成句 → 配音频 → 逐句听读。

以**反复精听、听懂真实英文对话**为目标。文本来源不限:Slack 聊天、会议纪要、英文博客、文档都行。

## 架构

Tauri 2(Rust core + WebView 前端 + 原生 JS + JSON 数据驱动 + 可配置 AI 提供者 + 本地缓存)。

```
任意文本粘贴
   │  [AI 清理四步:滤噪音 / 保结构 / 切句标注 / 翻译](复用 AIProvider)
   ▼
文档 JSON(嵌套块)── 唯一真身,跟用户选的 vault 文件夹走
   │  [edge-tts 批量生成句级 mp3](scripts/tts_generate.py,文件名 = 句 id)
   ▼
桌面层级折叠阅读器:点块/句播放 · 单句循环 · 调速 · 句间隔 · 懂了跳过 · 点词即查
```

### 目录结构

| 路径 | 作用 |
|---|---|
| `crates/core` | 平台无关核心:`note`(文档/块/句模型)+ `ai_provider`(OpenAI 兼容 chat 提供者) |
| `gui/src-tauri` | Tauri 后端命令:导入 / 音频 / vault / 即查 / 配置 |
| `gui/{index.html,main.js,styles.css}` | 前端阅读器(原生 JS) |
| `scripts/tts_generate.py` | edge-tts 句级 mp3 预处理脚本 |

### 存储(config 与 content 分离;文档树与音频树分离)

- **JSON 是真身**:每篇文档 = vault 内一个 `.json`,统一放在 `docs/` 下(可任意建子文件夹分类)。
- **音频**:统一放在 vault 级的 `media/` 下,按 `media/{note.id}/{音色}[-nospk]/{句序号}.{mp3|wav}` 组织。按 `note.id` 索引、与文档所在子文件夹解耦 —— 重组/移动文档不会让音频失联,不必重新生成。
- **配置**(provider / key / vault 路径 / TTS)留在 app 数据目录(`~/Library/Application Support/Attune`)。
- vault 像 Obsidian 一样随意选文件夹;换机/备份/同步只是搬一个文件夹。

```
我的英语库/                          ← 用户选的 vault
├── docs/                            ← 所有文档(可再分子文件夹)
│   └── 工作/SDK集成/
│       └── 2026-07-25 会议.json     ← 一篇文档(真身)
└── media/                           ← 所有音频,按 note.id 索引
    └── note_xxx/
        ├── zh_female_vv_uranus_bigtts/   ← 一个音色一个目录
        │   ├── 1.wav
        │   └── 2.wav
        └── en-US-AriaNeural-nospk/       ← 「不读姓名」变体
            └── 1.mp3
```

## 开发

```bash
# 前置:Rust(nightly/stable 1.85+),Node,pnpm,Python3 + edge-tts
pip install --index-url https://pypi.org/simple/ edge-tts   # 清华源没有,用官方源

cd gui
pnpm install        # 首次
pnpm tauri dev      # 启动开发
pnpm tauri build    # 打包
```

首次启动会自动弹出**设置**:选一个文件夹当 vault、配置 AI 提供商与 API Key(默认智谱 GLM,兼容任何 OpenAI 协议服务)、选 TTS 音色。

## 使用

1. **导入文本** → 粘贴英文原文(可选填标题/文件夹/来源)→ AI 自动清理切句翻译 → 自动生成逐句音频。
2. 阅读器里**点句/点块/听整篇**播放;传输栏切换 顺序 / 整篇循环 / 单句循环,调速、调句间隔。
3. 听懂的句点「懂了」(精听档自动跳过);任意单词**点一下即查**(带句子上下文,解决一词多义/行话)。

## 范围说明(本版本)

- ✅ 核心:AI 四步清理、句级 mp3、多粒度精听、懂了跳过、点词即查、Obsidian 式 vault。
- ⏭️ 未做(见需求文档第 8 节):记生词/词库/复习、局域网手机网页播放器、导出整篇 mp3、SQLite 检索索引、拖拽嵌套。

> 生词记录等进阶功能明确列在本版本范围外。即查(听读时点词弹释义)是做的,与"记词"是两码事。
