// Attune —— 英语听读工具前端(原生 JS)。
//
// 数据流:粘贴文本 → import_text(AI 四步清理)→ 文档 JSON 落盘 vault
//       → generate_audio(edge-tts 句级 mp3)→ 阅读器逐句精听 + 点词即查。
//
// 播放模型:queue = 一组按序播放的句。点句→queue=[该句];点块→该块句;听整篇→全部。
// loopMode:顺序(播完即停)/ 整篇(循环)/ 单句(反复)。
// skipMastered:精听时跳过已懂(构建 块/整篇 队列时排除 mastered);单独点句不受影响。

const tauriCore = window.__TAURI__?.core;
const invoke = tauriCore?.invoke?.bind(tauriCore) ?? (async (cmd) => {
    throw new Error(`浏览器预览模式不支持原生命令:${cmd}(请用 tauri dev 运行)`);
});
const tauriEvent = window.__TAURI__?.event;

// 预缓存:监听后端 precache-progress 事件显示「x/y 句」,invoke 结束后取消监听。
async function invokeGenerateAudio(rel, onProgress) {
    let unlisten = null;
    try {
        if (tauriEvent?.listen && onProgress) {
            unlisten = await tauriEvent.listen('precache-progress', (e) => {
                const p = e.payload || {};
                onProgress(p.done ?? 0, p.total ?? 0);
            });
        }
        return await invoke('generate_audio', {
            relPath: rel,
            voice: state.currentVoice || null,
            readSpeaker: state.readSpeaker,
        });
    } finally {
        if (unlisten) unlisten();
    }
}

const $ = (id) => document.getElementById(id);

// 应用内确认弹窗:返回 Promise<boolean>。替代原生 confirm(WKWebView 下不弹框、直接返回 true)。
function confirmDialog(message, { okText = '确定', danger = false } = {}) {
    return new Promise((resolve) => {
        const overlay = $('confirmModal'), ok = $('confirmOk'), cancel = $('confirmCancel');
        $('confirmMsg').textContent = message;
        ok.textContent = okText;
        ok.classList.toggle('danger', !!danger);
        overlay.hidden = false;
        const done = (v) => {
            overlay.hidden = true;
            ok.onclick = cancel.onclick = overlay.onclick = null;
            document.removeEventListener('keydown', onKey);
            resolve(v);
        };
        const onKey = (e) => {
            if (e.key === 'Escape') { e.preventDefault(); done(false); } // 只有 Esc=取消;确认必须点按钮
        };
        ok.onclick = () => done(true);
        cancel.onclick = () => done(false);
        overlay.onclick = (e) => { if (e.target === overlay) done(false); }; // 点遮罩=取消
        document.addEventListener('keydown', onKey);
        cancel.focus(); // 默认焦点在「取消」
    });
}

const state = {
    config: null,
    notes: [],            // NoteRef[]
    folders: [],          // 文件夹相对路径[](含空文件夹)
    note: null,           // 当前打开的 Note(完整)
    noteRel: '',          // 当前文档的 rel_path
    // 播放
    queue: [],            // Sentence[]
    qIndex: -1,
    loopMode: 'seq',      // seq | all | one
    skipMastered: true,
    speed: 0.75,
    gapMs: 0,
    showZh: false,
    playing: false,
    synthing: false,      // 是否正在实时合成当前句(显示「合成中…」+ 按钮转圈)
    advanceToken: 0,
    playToken: 0,         // 每次 playIndex 自增:异步合成回来后比对,过期的丢弃(防串音/多点)
    currentVoice: '',     // 当前音色(可听读中途切换);空=用 config 默认
    readSpeaker: true,    // 是否读说话人姓名(热开关);默认跟随 config
    audioCache: new Map(),// `${voice}:${readSpeaker}:${sentenceId}` -> dataURL
    audioInflight: new Map(), // 同键在途的合成 Promise:多次点击/预取复用一个,不重复起进程
    zhInflight: new Map(),    // 句 id -> 在途的补翻译 Promise(去重)
    zhChain: Promise.resolve(), // 补翻译串行队列:后端整篇读改写 JSON,并发会互相覆盖
    // 编辑
    forceRaw: false,      // 已转化文档里点「回原文重转」时为 true,强制原文 textarea
    structMode: false,    // 当前是否在句块就地编辑
    editBlocks: null,     // 句块编辑用的深拷贝
};

const audio = $('audio');

// ─────────────────────── 启动 ───────────────────────

async function init() {
    bindGlobal();
    bindDrag();
    bindTransport();
    bindImport();
    bindSettings();
    bindWordPopover();

    const cfg = await invoke('get_config');
    state.config = cfg;
    applyConfigToUI(cfg);

    await refreshTree();

    // 首次使用:没选库或没配 key,直接进设置页
    if (!cfg.vault_path || !cfg.has_api_key) {
        openSettings();
    }
}

// ─────────────────────── 配置 ───────────────────────

// 音色列表来自配置(可自行增删),深拷贝到本地便于编辑,保存时整体写回。
let ttsVoices = {};
const TTS_VOICE_HINTS = {
    edge: 'edge-tts 音色;需要更多点下方「＋ 添加」',
    macos: 'macOS 系统音色,离线免配置;高质量音色先在 系统设置→辅助功能→Read & Speak→System Voice→Manage Voices 下载,再点「扫描系统音色」',
    zhipu: '智谱 GLM-TTS;文档公开音色暂仅 female(彤彤),可自行追加',
    doubao: '火山 voice_type;豆包音色很多,按需「＋ 添加」(如 en_ 或 BV 前缀)',
    aliyun: '阿里云 NLS 发音人(如 xiaoyun / ailun);可自行追加',
};

function applyConfigToUI(cfg) {
    $('vaultLabel').textContent = cfg.vault_path ? cfg.vault_path.split('/').pop() : '未选择库';
    $('vaultPath').value = cfg.vault_path || '';
    $('openVaultBtn').disabled = !cfg.vault_path; // 未选库时禁用「打开目录」
    $('providerSel').value = cfg.provider;
    $('baseUrlInput').value = cfg.base_url;
    setModelOptions(cfg.model ? [cfg.model] : [], cfg.model);

    const tts = cfg.tts || {};
    $('ttsProviderSel').value = tts.provider || 'edge';
    $('ttsRate').value = tts.rate || '';
    state.readSpeaker = tts.read_speaker !== false;
    $('ttsReadSpeaker').checked = state.readSpeaker;
    $('playReadSpeakerChk').checked = state.readSpeaker;
    $('ttsDoubaoResource').value = tts.doubao_resource_id || 'seed-tts-2.0';
    $('ttsAliyunAppKey').value = tts.aliyun_app_key || '';
    $('ttsAliyunAkId').value = tts.aliyun_access_key_id || '';
    $('ttsAliyunRegion').value = tts.aliyun_region || 'cn-shanghai';
    $('ttsZhipuKey').placeholder = tts.has_zhipu_key ? '已保存(留空不改)' : '请输入智谱 TTS API Key';
    $('ttsDoubaoKey').placeholder = tts.has_doubao_key ? '已保存(留空不改)' : 'api-key-...';
    $('ttsAliyunAkSecret').placeholder = tts.has_aliyun_secret ? '已保存(留空不改)' : '输入阿里云 AccessKey Secret';
    ttsVoices = JSON.parse(JSON.stringify(tts.voices || {}));
    setTtsProviderUI(tts.provider || 'edge', tts.voice);
    // 阅读器音色下拉:默认跟随 config 音色,之后可听读中途切换
    state.currentVoice = tts.voice || '';
    populateReaderVoices(tts.provider || 'edge', state.currentVoice);

    $('configPathHint').textContent = '配置文件:' + cfg.config_path;
    $('apiKeyInput').placeholder = cfg.has_api_key ? '已保存(留空不改)' : '输入 API Key';
}

// 按厂商填充音色下拉(数据来自 ttsVoices,即配置)。
function populateTtsVoices(provider, selected) {
    const sel = $('ttsVoice');
    const list = ttsVoices[provider] || [];
    sel.innerHTML = list.map(([id, name]) => `<option value="${esc(id)}">${esc(name)}</option>`).join('')
        || '<option value="">(无,请添加)</option>';
    sel.value = selected && list.some(([id]) => id === selected) ? selected : (list[0] && list[0][0]) || '';
}

// 阅读器工具栏的音色下拉:列当前厂商的音色,选中值 = state.currentVoice。
function populateReaderVoices(provider, selected) {
    const sel = $('voiceSel');
    if (!sel) return;
    const list = ttsVoices[provider] || [];
    sel.innerHTML = list.map(([id, name]) => `<option value="${esc(id)}">${esc(name)}</option>`).join('')
        || '<option value="">(无音色,去设置添加)</option>';
    const val = selected && list.some(([id]) => id === selected) ? selected : (list[0] && list[0][0]) || '';
    sel.value = val;
    state.currentVoice = val;
}

// 显隐凭证组 + 填充音色。onProviderChange 时不传 savedVoice(切厂商重置为该厂商第一个)。
function setTtsProviderUI(provider, savedVoice) {
    document.querySelectorAll('.tts-only').forEach((el) => { el.style.display = 'none'; });
    if (provider === 'zhipu') document.querySelectorAll('.tts-zhipu').forEach((el) => { el.style.display = ''; });
    if (provider === 'doubao') document.querySelectorAll('.tts-doubao').forEach((el) => { el.style.display = ''; });
    if (provider === 'aliyun') document.querySelectorAll('.tts-aliyun').forEach((el) => { el.style.display = ''; });
    if (provider === 'macos') document.querySelectorAll('.tts-macos').forEach((el) => { el.style.display = ''; });
    $('ttsVoiceHint').textContent = TTS_VOICE_HINTS[provider] || '';
    populateTtsVoices(provider, savedVoice);
}

// 音色增删(写本地 ttsVoices,保存时整体回写配置)
function addTtsVoice() {
    const id = $('ttsVoiceAddId').value.trim();
    if (!id) return;
    const name = $('ttsVoiceAddName').value.trim() || id;
    const p = $('ttsProviderSel').value;
    if (!ttsVoices[p]) ttsVoices[p] = [];
    if (!ttsVoices[p].some(([v]) => v === id)) ttsVoices[p].push([id, name]);
    $('ttsVoiceAddId').value = '';
    $('ttsVoiceAddName').value = '';
    populateTtsVoices(p, id);
}
async function delTtsVoice() {
    const p = $('ttsProviderSel').value;
    const cur = $('ttsVoice').value;
    if (!cur) return;
    if (!(await confirmDialog(`删除音色「${cur}」?`, { okText: '删除', danger: true }))) return;
    ttsVoices[p] = (ttsVoices[p] || []).filter(([v]) => v !== cur);
    populateTtsVoices(p, (ttsVoices[p][0] && ttsVoices[p][0][0]) || '');
}

// 扫描本机 macOS 系统英文音色(后端解析 `say -v '?'`),替换 ttsVoices.macos;
// 需先保存设置才写入 config.json。当前选中项若还在则保持。
async function scanSystemVoices() {
    const btn = $('ttsScanVoicesBtn');
    const status = $('ttsScanStatus');
    btn.disabled = true;
    status.textContent = '扫描中…';
    try {
        const list = await invoke('list_system_voices');
        if (!list.length) {
            status.textContent = '没扫到英文系统音色';
            return;
        }
        ttsVoices['macos'] = list;
        const cur = $('ttsVoice').value;
        const keep = list.some(([id]) => id === cur) ? cur : list[0][0];
        populateTtsVoices('macos', keep);
        status.textContent = `扫到 ${list.length} 个英文音色(Premium 优先);保存设置后生效`;
    } catch (e) {
        status.textContent = `扫描失败: ${e}`;
    } finally {
        btn.disabled = false;
    }
}

function shortenPath(p) {
    const parts = p.split('/');
    if (parts.length <= 3) return p;
    return '…/' + parts.slice(-2).join('/');
}

// ─────────────────────── 文档库 / 文件夹树 ───────────────────────

async function refreshTree() {
    if (!state.config?.vault_path) {
        $('tree').innerHTML = '<div class="tree-empty">先到「设置」选择一个文件夹作为你的英语库</div>';
        return;
    }
    try {
        const [notes, folders] = await Promise.all([invoke('list_vault'), invoke('list_folders')]);
        state.notes = notes;
        state.folders = folders;
        renderTree(notes, folders);
    } catch (e) {
        $('tree').innerHTML = `<div class="tree-empty">读取库失败:${esc(e)}</div>`;
    }
}

// 新建文件夹:点击图标 → 显示内联输入框 → 回车创建 / Esc 取消
// 在指定父目录下新建文件夹:复用顶部输入框,预填父路径,用户续写子目录名。
// parent='' = 在根下建。create_folder 后端按 '/' 分段建嵌套目录,所以支持多级。
function newFolderIn(parent) {
    if (!state.config?.vault_path) { alert('请先在设置里选择库目录'); openSettings(); return; }
    const input = $('newFolderInput');
    input.hidden = false;
    input.value = parent ? parent + '/' : '';
    input.focus();
    const end = input.value.length;
    input.setSelectionRange(end, end); // 光标移到末尾,直接续写子目录名
}
function newFolder() { newFolderIn(''); } // 顶栏「📁＋」→ 在根建

async function doCreateFolder() {
    const input = $('newFolderInput');
    const name = input.value.trim();
    input.hidden = true;
    input.value = '';
    if (!name) return;
    try {
        await invoke('create_folder', { name });
        await refreshTree();
    } catch (e) { alert('创建失败: ' + e); }
}

// 删除文件夹(含其中所有文档与音频)。node 是树节点,含 path/notes/folders。
async function deleteFolder(node) {
    const countNotes = (n) => (n.notes ? n.notes.length : 0)
        + Object.values(n.folders || {}).reduce((a, f) => a + countNotes(f), 0);
    const cnt = countNotes(node);
    const msg = cnt > 0
        ? `删除文件夹「${node.name}」及其中 ${cnt} 篇文档(含音频)?此操作不可撤销。`
        : `删除空文件夹「${node.name}」?`;
    if (!(await confirmDialog(msg, { okText: '删除', danger: true }))) return;
    try {
        await invoke('delete_folder', { relPath: node.path });
    } catch (e) { alert('删除失败:' + e); return; }
    // 当前打开的文档若在被删文件夹里 → 回到欢迎页
    if (state.noteRel && (state.noteRel === node.path || state.noteRel.startsWith(node.path + '/'))) {
        stopPlayback();
        state.noteRel = ''; state.note = null;
        $('readerView').hidden = true; $('editorView').hidden = true; $('transport').hidden = true;
        $('emptyState').hidden = false;
    }
    await refreshTree();
}

// 文件夹悬停操作的描边图标(与顶栏风格一致:14×14、currentColor 描边)
const ICON_DOC_PLUS = '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M8.5 2H4a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1V6.5z"/><path d="M8.5 2v4.5H13"/><line x1="8" y1="8.5" x2="8" y2="12"/><line x1="6.25" y1="10.25" x2="9.75" y2="10.25"/></svg>';
const ICON_FOLDER_PLUS = '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4.5h3.5L7 6.5h7V13a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z"/><line x1="8" y1="8" x2="8" y2="11.5"/><line x1="6.3" y1="9.75" x2="9.7" y2="9.75"/></svg>';
const ICON_TRASH = '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M3 4.5h10"/><path d="M5.5 4.5V3.2a1 1 0 0 1 1-1h3a1 1 0 0 1 1 1v1.3"/><path d="M4.2 4.5 4.8 13a1 1 0 0 0 1 .9h4.4a1 1 0 0 0 1-.9l.6-8.5"/></svg>';

// 折叠态(按文件夹 path 记),重渲染时保留
const collapsedFolders = new Set();

function renderTree(notes, folders) {
    const root = $('tree');
    if (!notes.length && !folders.length) {
        root.innerHTML = '<div class="tree-empty">库还是空的。点「＋ 新建文档」或「📁＋」建文件夹开始。</div>';
        return;
    }
    // 构建嵌套树:先用所有文件夹(含空的)建节点,再放文档进去
    const tree = { folders: {}, notes: [] };
    const ensure = (folderPath) => {
        const parts = String(folderPath).split('/').map((s) => s.trim()).filter(Boolean);
        let cur = tree;
        let acc = '';
        for (const p of parts) {
            acc = acc ? acc + '/' + p : p;
            if (!cur.folders[p]) cur.folders[p] = { folders: {}, notes: [], name: p, path: acc };
            cur = cur.folders[p];
        }
        return cur;
    };
    (folders || []).forEach(ensure);
    for (const n of notes) {
        const cur = n.folder ? ensure(n.folder) : tree;
        cur.notes.push(n);
    }
    root.innerHTML = '';
    const frag = document.createDocumentFragment();
    // 根目录节点始终显示,方便在根下直接「＋文档 / ＋子目录」
    frag.appendChild(renderFolderNode({ folders: {}, notes: tree.notes, name: '根目录', path: '' }));
    for (const fname of Object.keys(tree.folders).sort()) {
        frag.appendChild(renderFolderNode(tree.folders[fname]));
    }
    root.appendChild(frag);
    bindTreeEvents(root);
}

function byTitle(a, b) {
    return (a.title || '').localeCompare(b.title || '');
}

function renderFolderNode(node) {
    const collapsed = collapsedFolders.has(node.path);
    const div = el('div', 'folder' + (collapsed ? ' collapsed' : ''));
    const childNotes = [...node.notes].sort(byTitle);
    const childFolders = Object.keys(node.folders).sort();
    const count = childNotes.length + childFolders.length;
    const head = el('div', 'folder-head');
    head.innerHTML = `<span class="caret">▾</span><span class="fname">${esc(node.name)}</span><span class="note-sub">${count}</span>`;
    // 就地折叠:只切 class,不重渲染(保留滚动/hover,Obsidian 式)
    head.addEventListener('click', () => {
        const nowCollapsed = div.classList.toggle('collapsed');
        if (nowCollapsed) collapsedFolders.add(node.path);
        else collapsedFolders.delete(node.path);
    });
    // 悬停操作:在此目录新建文档 / 新建子目录(node.path='' 即根目录)
    const acts = el('div', 'folder-acts');
    const addDoc = el('button', 'icon-button sm');
    addDoc.title = '在此目录新建文档';
    addDoc.innerHTML = ICON_DOC_PLUS;
    addDoc.onclick = (e) => { e.stopPropagation(); newDocIn(node.path); };
    const addSub = el('button', 'icon-button sm');
    addSub.title = '在此目录新建子文件夹';
    addSub.innerHTML = ICON_FOLDER_PLUS;
    addSub.onclick = (e) => { e.stopPropagation(); newFolderIn(node.path); };
    acts.appendChild(addDoc); acts.appendChild(addSub);
    if (node.path) { // 根目录不可删
        const del = el('button', 'icon-button sm danger');
        del.title = '删除此文件夹(含其中所有文档与音频)';
        del.innerHTML = ICON_TRASH;
        del.onclick = (e) => { e.stopPropagation(); deleteFolder(node); };
        acts.appendChild(del);
    }
    head.appendChild(acts);
    div.appendChild(head);
    const items = el('div', 'folder-items');
    for (const n of childNotes) items.appendChild(noteItemEl(n));
    for (const fn of childFolders) items.appendChild(renderFolderNode(node.folders[fn]));
    div.appendChild(items);
    return div;
}

function noteItemEl(n) {
    const div = el('div', 'note-item' + (n.rel_path === state.noteRel ? ' active' : ''));
    div.dataset.rel = n.rel_path;
    div.title = n.rel_path;
    // 未转化=草稿;已转化但未预缓存默认音色=「未缓存」(仍可实时合成听读,只是没离线)。
    const badge = !n.converted
        ? '<span class="draft">草稿</span>'
        : !n.audio_ready
        ? '<span class="noaudio">未缓存</span>'
        : '';
    const mastered = n.converted && n.mastered ? `<span class="note-sub">懂${n.mastered}/${n.readable}</span>` : '';
    div.innerHTML = `<span class="ntitle">${esc(n.title)}</span>${badge}${mastered}<button class="icon-button sm note-del" title="删除">×</button>`;
    return div;
}

// 就地切换选中高亮(不重渲染树)
function setActiveNote(rel) {
    document.querySelectorAll('.note-item.active').forEach((el) => el.classList.remove('active'));
    if (rel) {
        const el = document.querySelector(`.note-item[data-rel="${cssEscape(rel)}"]`);
        if (el) el.classList.add('active');
    }
}

function bindTreeEvents(root) {
    root.querySelectorAll('.note-item').forEach((it) => {
        // 点击即时高亮 + 打开(Obsidian 式即时反馈)
        it.addEventListener('click', () => {
            setActiveNote(it.dataset.rel);
            openNote(it.dataset.rel);
        });
    });
    root.querySelectorAll('.note-del').forEach((b) => {
        b.addEventListener('click', async (e) => {
            e.stopPropagation();
            const rel = b.closest('.note-item').dataset.rel;
            if (await confirmDialog('删除这篇文档?(含其音频)', { okText: '删除', danger: true })) {
                try { await invoke('delete_note', { relPath: rel }); } catch (err) { alert(err); }
                if (state.noteRel === rel) {
                    stopPlayback();
                    state.noteRel = ''; state.note = null;
                    $('readerView').hidden = true; $('editorView').hidden = true; $('transport').hidden = true;
                    $('emptyState').hidden = false;
                }
                await refreshTree();
            }
        });
    });
}

// ─────────────────────── 打开 / 渲染文档 ───────────────────────

async function openNote(rel, opts = {}) {
    stopPlayback();
    try {
        const note = await invoke('load_note', { relPath: rel });
        state.note = note;
        state.noteRel = rel;
        state.forceRaw = false;       // 打开文档默认不强制原文模式
        $('emptyState').hidden = true;
        // 以「有没有句块」为准(老文档 converted 字段可能缺失):有句块=阅读器(家),没有=草稿进编辑器
        const hasBlocks = (note.blocks || []).length > 0;
        const editMode = opts.forceEditor || !hasBlocks;
        setViewMode(editMode);
        setActiveNote(rel); // 就地高亮,不重渲染树
        if (!editMode) warmFirstSentences(); // 开篇预热:后台先合成前几句,按播放即秒开
    } catch (e) {
        alert('打开失败:' + e);
    }
}

// 切换 阅读器 / 编辑器。编辑器内分两态:已转化→就地句块编辑;草稿/回原文→原文 textarea。
function setViewMode(editMode) {
    const hasBlocks = state.note && (state.note.blocks || []).length > 0;
    const showReader = !editMode && hasBlocks;
    $('readerView').hidden = !showReader;
    $('editorView').hidden = !editMode;
    $('transport').hidden = !showReader;
    if (!editMode) { renderNote(state.note); return; }
    $('editTitle').value = state.note?.title || '';
    const useStruct = hasBlocks && !state.forceRaw;
    state.structMode = useStruct;
    $('structEditor').hidden = !useStruct;
    $('rawEditor').hidden = !!useStruct;
    // 顶部按钮:句块模式→「更多」(整篇重转收里面)+ 返回阅读;原文模式→AI 转化
    $('editorMoreBtn').hidden = !useStruct;  // 「更多」仅句块模式出现
    $('toRawBtn').hidden = true;             // 回原文重转默认藏进「更多」,点 ⋯ 才显示
    $('editorBackBtn').hidden = !useStruct; // 句块模式(已转化)才显示返回阅读
    $('convertBtn').hidden = !!useStruct;
    if (useStruct) fillStructEditor();
    else fillRawEditor();
}

// 原文编辑器(草稿 / 回原文重转)
function fillRawEditor() {
    $('editRaw').value = state.note?.raw || '';
    lastSavedTitle = (state.note?.title || '').trim();
    const hasBlocks = (state.note?.blocks || []).length > 0;
    const s = $('editorStatus');
    s.className = 'field-note';
    s.textContent = hasBlocks
        ? '原文模式。改完点「AI 转化」整体重跑(会覆盖当前句块与音频)。'
        : '草稿。粘贴/编辑原文,删掉噪音,再点「AI 转化」。';
    setTimeout(() => { try { $('editRaw').focus(); } catch (_) {} }, 30);
}

// 就地句块编辑器(已转化,直接改、不经 AI)
function fillStructEditor() {
    state.editBlocks = JSON.parse(JSON.stringify(state.note?.blocks || []));
    renderStructBlocks();
    setStructStatus('直接改英文即可(自动保存,不经 AI)。改过的句音频会失效,听读前重新合成。删句用 ×;说话人 / 朗读点 ⋯。', '');
}

function setStructStatus(text, kind) {
    const s = $('editorStatus');
    s.className = 'field-note' + (kind ? ' ' + kind : '');
    s.textContent = text;
}

function renderStructBlocks() {
    const host = $('structBlocks');
    host.innerHTML = '';
    (state.editBlocks || []).forEach((block) => {
        const div = el('div', 'struct-block');
        div.appendChild(el('div', 'struct-block-type', block.type === 'list' ? '列表' : '段落'));
        const groups = block.type === 'list'
            ? (block.items || []).map((it) => it.sentences || [])
            : [block.sentences || []];
        groups.forEach((sents) => {
            [...sents].forEach((s) => {
                div.appendChild(structCard(s, () => {
                    const i = sents.indexOf(s);
                    if (i >= 0) sents.splice(i, 1);
                    renderStructBlocks();
                    scheduleSave();
                }));
            });
        });
        host.appendChild(div);
    });
}

// 编辑卡片瘦身:核心只有「改英文 + 删句」。中文由 AI 自动产出,不再手编辑;
// 说话人 / 朗读属于偶尔才用,收进卡片「⋯」里,默认折叠。
function structCard(s, onDelete) {
    const card = el('div', 'sedit-card');

    const en = document.createElement('textarea');
    en.className = 'sedit-en'; en.value = s.en || ''; en.rows = 1;
    en.addEventListener('input', () => { s.en = en.value; autoGrow(en); scheduleSave(); });

    // 次要设置(说话人 / 朗读):默认收起,点「⋯」展开
    const detail = el('div', 'sedit-detail'); detail.hidden = true;
    const speaker = document.createElement('input');
    speaker.className = 'speaker'; speaker.value = s.speaker || ''; speaker.placeholder = '说话人(可空)';
    speaker.addEventListener('input', () => { s.speaker = speaker.value; scheduleSave(); });
    const ra = el('label', 'read-aloud');
    ra.innerHTML = '<input type="checkbox"><span>朗读</span>';
    const raChk = ra.querySelector('input'); raChk.checked = !!s.read_aloud;
    raChk.addEventListener('change', () => { s.read_aloud = raChk.checked; scheduleSave(); });
    detail.append(speaker, ra);

    const tools = el('div', 'sedit-tools');
    const moreBtn = el('button', 'icon-button sm'); moreBtn.textContent = '⋯'; moreBtn.title = '说话人 / 朗读';
    moreBtn.addEventListener('click', () => {
        detail.hidden = !detail.hidden;
        moreBtn.classList.toggle('on', !detail.hidden);
    });
    const del = el('button', 'icon-button sm del'); del.textContent = '×'; del.title = '删除该句';
    del.addEventListener('click', onDelete);
    tools.append(moreBtn, del);

    const main = el('div', 'sedit-main');
    main.append(en, tools);

    card.append(main, detail);
    setTimeout(() => autoGrow(en), 0);
    return card;
}

function autoGrow(ta) { ta.style.height = 'auto'; ta.style.height = ta.scrollHeight + 'px'; }

function findRel(id) {
    const r = state.notes.find((n) => n.id === id);
    return r ? r.rel_path : null;
}

function renderNote(note) {
    $('noteTitle').textContent = note.title;
    const counts = sentenceCounts(note);
    const meta = [];
    meta.push(`<span>${note.created_at}</span>`);
    meta.push(`<span>${counts.readable} 句可听</span>`);
    if (note.source) meta.push(`<span>${esc(note.source)}</span>`);
    (note.tags || []).forEach((t) => meta.push(`<span class="tag">${esc(t)}</span>`));
    $('noteMeta').innerHTML = meta.join('');

    // 预缓存按钮常驻:实时合成已能直接听,这里只是可选的「离线预缓存当前音色」。
    const genBtn = $('genAudioBtn');
    genBtn.hidden = false;
    genBtn.onclick = () => generateAudio(state.noteRel);

    const host = $('blocks');
    host.innerHTML = '';
    for (const block of note.blocks) {
        if (block.type === 'paragraph') {
            host.appendChild(renderParagraph(block));
        } else if (block.type === 'list') {
            host.appendChild(renderList(block));
        }
    }
}

function renderParagraph(block) {
    const div = el('div', 'block paragraph');
    const sentences = block.sentences || [];
    // 块级「听这部分」
    const head = el('div', 'block-head');
    const playBtn = el('button', 'block-play', '▷ 听这段');
    playBtn.onclick = () => playRange(sentences.map((s) => s.id));
    head.appendChild(playBtn);
    div.appendChild(head);
    for (const s of sentences) div.appendChild(renderSentence(s));
    return div;
}

function renderList(block) {
    const div = el('div', 'block list');
    const head = el('div', 'block-head');
    const allSentences = [];
    for (const item of (block.items || [])) for (const s of (item.sentences || [])) allSentences.push(s);
    const playBtn = el('button', 'block-play', '▷ 听整个列表');
    playBtn.onclick = () => playRange(allSentences.map((s) => s.id));
    head.appendChild(playBtn);
    div.appendChild(head);
    for (const item of (block.items || [])) {
        for (const s of (item.sentences || [])) {
            const row = renderSentence(s);
            // 加列表项符号
            const bullet = el('span', 'bullet', '•');
            row.insertBefore(bullet, row.firstChild);
            div.appendChild(row);
        }
    }
    return div;
}

function renderSentence(s) {
    const row = el('div', 'sentence');
    row.dataset.id = s.id;
    if (s.mastered) row.classList.add('mastered');
    if (state.showZh) row.classList.add('show-zh');

    const play = el('button', 's-play', '▶');
    if (!s.read_aloud || !s.audio) {
        play.disabled = true;
        play.style.opacity = '0.3';
        play.textContent = '–';
    } else {
        play.onclick = (e) => { e.stopPropagation(); playRange([s.id], { forceLoopOne: false }); };
    }
    row.appendChild(play);

    const body = el('div', 's-body');
    const en = el('div', 'en');
    en.innerHTML = sentenceEnHtml(s);
    en.addEventListener('click', (ev) => {
        if (ev.target.classList.contains('word')) {
            ev.stopPropagation();
            showWordPopover(ev.target, s);
        } else if (s.read_aloud && s.audio) {
            playRange([s.id]);
        }
    });
    body.appendChild(en);

    const zh = el('div', 'zh', s.zh || '');
    body.appendChild(zh);

    const tools = el('div', 's-tools');
    const zhBtn = el('button', 's-tool' + (row.classList.contains('show-zh') ? ' on' : ''), '译');
    zhBtn.onclick = async (e) => {
        e.stopPropagation();
        if (zhBtn.disabled) return;
        const opening = !row.classList.contains('show-zh');
        // 没译文 → 打开时先 AI 补翻译(落盘,不动音频);预翻译已在途则直接复用
        if (opening && !(s.zh || '').trim()) {
            zhBtn.disabled = true; zhBtn.textContent = '翻译中…';
            try {
                await ensureZh(s, { force: true });
            } catch (err) {
                alert('翻译失败:' + err);
                return;
            } finally {
                zhBtn.disabled = false; zhBtn.textContent = '译';
            }
        }
        row.classList.toggle('show-zh'); zhBtn.classList.toggle('on');
    };
    const optBtn = el('button', 's-tool', 'AI优化');
    optBtn.title = '用 AI 校对转写错误(如 Web View→WebView),预览后再采纳';
    optBtn.onclick = (e) => { e.stopPropagation(); optimizeSentence(s, row, optBtn); };
    const masBtn = el('button', 's-tool' + (s.mastered ? ' on' : ''), s.mastered ? '已懂 ✓' : '懂了');
    masBtn.onclick = async (e) => {
        e.stopPropagation();
        await toggleMastered(s.id, !s.mastered);
    };
    tools.appendChild(zhBtn); tools.appendChild(optBtn); tools.appendChild(masBtn);
    body.appendChild(tools);

    row.appendChild(body);
    return row;
}

// 单句 AI 优化:请求建议 → 预览面板(原/新/译文 + 备注输入)→ 采纳则改 en/zh、作废该句音频、重渲染。
async function optimizeSentence(s, row, btn) {
    if (btn.disabled) return;
    row.querySelector('.s-opt-preview')?.remove();
    const orig = btn.textContent;
    btn.disabled = true; btn.textContent = '优化中…';
    try {
        const res = await invoke('optimize_sentence', { relPath: state.noteRel, sentenceId: s.id, hint: null });
        showOptPreview(s, row, res, '');
    } catch (e) {
        alert('AI 优化失败:' + e);
    } finally {
        btn.disabled = false; btn.textContent = orig;
    }
}

// 词级 diff 高亮:把「原/新」英文按空白切成 token,用最长公共子序列(LCS)对齐。
// 原句被改掉的词 → .diff-del(红 + 删除线),新句换上的词 → .diff-ins(绿)。
// 一眼看出 AI 到底动了哪几个词。返回 [oldHtml, newHtml];句长几十词内,O(n·m) 足够快。
function wordDiffMarkup(oldText, newText) {
    const a = String(oldText || '').split(/(\s+)/).filter((t) => t !== '');
    const b = String(newText || '').split(/(\s+)/).filter((t) => t !== '');
    const n = a.length, m = b.length;
    // lcs[i][j] = a[i..] 与 b[j..] 的最长公共子序列长度(倒序填表,便于回溯)
    const lcs = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
    for (let i = n - 1; i >= 0; i--) {
        for (let j = m - 1; j >= 0; j--) {
            lcs[i][j] = a[i] === b[j]
                ? lcs[i + 1][j + 1] + 1
                : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
        }
    }
    // 回溯:公共 token 原样输出;只在差异处打标记
    let oh = '', nh = '', i = 0, j = 0;
    while (i < n && j < m) {
        if (a[i] === b[j]) { oh += esc(a[i]); nh += esc(b[j]); i++; j++; }
        else if (lcs[i + 1][j] >= lcs[i][j + 1]) { oh += `<span class="diff-del">${esc(a[i])}</span>`; i++; }
        else { nh += `<span class="diff-ins">${esc(b[j])}</span>`; j++; }
    }
    while (i < n) { oh += `<span class="diff-del">${esc(a[i++])}</span>`; }
    while (j < m) { nh += `<span class="diff-ins">${esc(b[j++])}</span>`; }
    return [oh, nh];
}

// 渲染/刷新优化预览面板。res=当前建议;prefillHint=回填上次输入的提示,便于继续微调。
function showOptPreview(s, row, res, prefillHint) {
    const body = row.querySelector('.s-body');
    body.querySelector('.s-opt-preview')?.remove();

    const [oldHtml, newHtml] = wordDiffMarkup(s.en, res.en); // 词级高亮:红删/绿增
    const box = el('div', 's-opt-preview');
    const oldLine = el('div', 's-opt-old'); oldLine.innerHTML = `<b>原</b> ${oldHtml}`;
    const newLine = el('div', 's-opt-new'); newLine.innerHTML = `<b>新</b> ${newHtml}`;
    const zhLine = el('div', 's-opt-zh', res.zh || '');
    if (!res.changed) newLine.innerHTML += ' <span class="s-opt-note">(AI 认为无需改动,可在下方补充提示重试)</span>';

    // 备注提示 + 按提示重优化
    const hintRow = el('div', 's-opt-hintrow');
    const hintInput = el('input', 's-opt-hint');
    hintInput.type = 'text';
    hintInput.placeholder = '补充提示,如:loader G s 其实是 loader.js';
    hintInput.value = prefillHint || '';
    const reBtn = el('button', 's-tool', '按提示重优化');
    hintRow.appendChild(hintInput); hintRow.appendChild(reBtn);

    const acts = el('div', 's-opt-acts');
    const accept = el('button', 's-tool primary', '采纳');
    const reject = el('button', 's-tool', '不改');
    acts.appendChild(accept); acts.appendChild(reject);

    box.append(oldLine, newLine, zhLine, hintRow, acts);
    body.appendChild(box);

    const reoptimize = async () => {
        const h = hintInput.value.trim();
        reBtn.disabled = true; reBtn.textContent = '重优化中…';
        try {
            const r2 = await invoke('optimize_sentence', { relPath: state.noteRel, sentenceId: s.id, hint: h || null });
            showOptPreview(s, row, r2, h); // 重建面板并保留提示
        } catch (err) {
            alert('重优化失败:' + err);
            reBtn.disabled = false; reBtn.textContent = '按提示重优化';
        }
    };
    reBtn.onclick = (e) => { e.stopPropagation(); reoptimize(); };
    hintInput.addEventListener('keydown', (ev) => { if (ev.key === 'Enter') { ev.preventDefault(); reoptimize(); } });
    hintInput.addEventListener('click', (e) => e.stopPropagation());

    accept.onclick = async (e) => {
        e.stopPropagation();
        accept.disabled = reject.disabled = true;
        try {
            await invoke('apply_sentence_edit', { relPath: state.noteRel, sentenceId: s.id, en: res.en, zh: res.zh });
        } catch (err) { alert('采纳失败:' + err); accept.disabled = reject.disabled = false; return; }
        s.en = res.en; s.zh = res.zh;                       // 同步内存(state.note 引用)
        for (const k of [...state.audioCache.keys()]) {     // 清掉该句各音色缓存
            if (k.endsWith(':' + s.id)) state.audioCache.delete(k);
        }
        row.replaceWith(renderSentence(s));                 // 重渲染这一句(en 分词 + zh 更新)
    };
    reject.onclick = (e) => { e.stopPropagation(); box.remove(); };
}

// 把句子英文渲染成可点词:<span class="speaker">Dan:</span> <span class="word">word</span> ...
// 占位符 [链接]/[截图] 包成 .ph(不可点)
function sentenceEnHtml(s) {
    let html = '';
    if (s.speaker) html += `<span class="speaker">${esc(s.speaker)}:</span> `;
    const text = s.en || '';
    // 先把占位符 [..] 保护起来
    const parts = text.split(/(\[[^\]]+\])/g);
    for (const part of parts) {
        if (/^\[[^\]]+\]$/.test(part)) {
            html += `<span class="ph">${esc(part)}</span> `;
        } else {
            // 按空白拆词,每个 token 包成 .word
            const toks = part.split(/(\s+)/);
            for (const tk of toks) {
                if (/^\s+$/.test(tk) || tk === '') { html += tk; continue; }
                const clean = tk.replace(/^[^A-Za-z0-9']+|[^A-Za-z0-9']+$/g, '');
                if (clean) {
                    const lead = tk.slice(0, tk.indexOf(clean));
                    const trail = tk.slice(tk.indexOf(clean) + clean.length);
                    html += lead + `<span class="word" data-word="${esc(clean.toLowerCase())}">${esc(clean)}</span>` + trail;
                } else {
                    html += `<span>${esc(tk)}</span>`;
                }
            }
        }
    }
    return html;
}

function sentenceCounts(note) {
    let total = 0, readable = 0, mastered = 0;
    for (const s of allSentences(note)) {
        total++;
        if (s.read_aloud) readable++;
        if (s.mastered) mastered++;
    }
    return { total, readable, mastered };
}

function allSentences(note) {
    const out = [];
    for (const b of note.blocks) {
        if (b.type === 'paragraph') for (const s of (b.sentences || [])) out.push(s);
        else if (b.type === 'list') for (const it of (b.items || [])) for (const s of (it.sentences || [])) out.push(s);
    }
    return out;
}

function findSentence(note, id) {
    return allSentences(note).find((s) => s.id === id);
}

// ─────────────────────── 播放引擎 ───────────────────────

// 用一组句 id 构建队列并从头播放。range=all 时尊重 skipMastered。
function playRange(ids, opts = {}) {
    let list = ids.map((id) => findSentence(state.note, id)).filter(Boolean);
    if (opts.wholeDoc && state.skipMastered) {
        list = list.filter((s) => !s.mastered);
    }
    if (!list.length) {
        // 全被跳过,提示
        setNowPlaying('没有可播放的句(可能都已掌握)');
        return;
    }
    state.queue = list;
    state.qIndex = 0;
    playIndex(0);
}

function playAllDoc() {
    const ids = allSentences(state.note).filter((s) => s.read_aloud).map((s) => s.id);
    playRange(ids, { wholeDoc: true });
}

async function playIndex(i) {
    if (i < 0 || i >= state.queue.length) {
        stopPlayback();
        return;
    }
    state.qIndex = i;
    const s = state.queue[i];
    highlightSentence(s.id);
    setNowPlayingFromSentence(s);
    prefetchZh();

    if (!s.read_aloud) {
        // 不朗读(占位符/链接等):直接跳下一句
        scheduleAdvance(0);
        return;
    }
    const token = ++state.playToken; // 本次播放请求;合成回来后比对,过期则丢弃
    try {
        const cached = state.audioCache.has(audioKey(s.id));
        if (!cached) setSynthing(true, s);      // 未命中→显示「合成中…」,给用户即时反馈
        const url = await ensureAudio(s);
        if (token !== state.playToken) return;  // 期间用户切了句/停了,丢弃这次结果
        setSynthing(false);
        audio.src = url;
        audio.playbackRate = state.speed;
        await audio.play();
        if (token !== state.playToken) return;
        state.playing = true;
        setPlayBtn(true);
        preloadNext();
    } catch (e) {
        if (token !== state.playToken) return;
        setSynthing(false);
        setNowPlaying(`合成失败:${e}`);
        scheduleAdvance(600);
    }
}

// 合成中状态:播放键转圈 + 现在播放区标「合成中…」。给用户明确反馈,避免「点了没反应」。
function setSynthing(on, s) {
    state.synthing = on;
    const btn = $('playBtn');
    btn.classList.toggle('loading', on);
    if (on) {
        btn.innerHTML = '<span class="btn-spinner"></span>';
        const cur = s || state.queue[state.qIndex];
        if (cur) setNowPlaying(`${nowPlayingText(cur)}　·　合成中…`);
    } else {
        btn.textContent = state.playing ? '⏸' : '▶';
    }
}

// 音频内存缓存键:音色 + 读名字 + 句 id(与后端缓存目录维度一致)。
function audioKey(id) {
    return `${state.currentVoice || ''}:${state.readSpeaker ? 1 : 0}:${id}`;
}

// 热切换(音色 / 读名字):立即对后续句生效;若正在播,用新设定重播当前句
//(命中缓存则秒切,未命中会实时合成,略等)。
function reapplyPlayback(hint) {
    if (state.playing && state.qIndex >= 0 && state.qIndex < state.queue.length) {
        if (!state.audioCache.has(audioKey(state.queue[state.qIndex].id))) setNowPlaying(hint);
        playIndex(state.qIndex);
    }
}

function switchVoice(voice) {
    state.currentVoice = voice || '';
    reapplyPlayback('切换音色,合成中…');
}

// 取某句在「当前音色 + 读名字设定」下的音频:命中内存缓存直接返回;否则走 play_sentence
//(后端:磁盘缓存命中就读、未命中实时合成)。缓存键含这两个维度,切换互不覆盖。
async function ensureAudio(s) {
    const key = audioKey(s.id);
    if (state.audioCache.has(key)) return state.audioCache.get(key);
    // 同一句已在合成中(点了多次 / 预取撞上):复用同一个 Promise,不再起第二个 python 进程。
    if (state.audioInflight.has(key)) return state.audioInflight.get(key);
    const p = invoke('play_sentence', {
        relPath: state.noteRel,
        sentenceId: s.id,
        voice: state.currentVoice || null,
        readSpeaker: state.readSpeaker,
    }).then((url) => {
        state.audioCache.set(key, url);
        state.audioInflight.delete(key);
        return url;
    }).catch((e) => {
        state.audioInflight.delete(key);
        throw e;
    });
    state.audioInflight.set(key, p);
    return p;
}

// 预取深度:提前合成后面几句,用「正在播这句」的时间盖住「合成后面几句」的网络延迟。
const PREFETCH_AHEAD = 3;
function preloadNext() {
    for (let k = 1; k <= PREFETCH_AHEAD; k++) {
        const nx = state.queue[state.qIndex + k];
        if (!nx) break;
        if (!nx.read_aloud) continue;
        const key = audioKey(nx.id);
        if (!state.audioCache.has(key) && !state.audioInflight.has(key)) {
            ensureAudio(nx).catch(() => {});
        }
    }
}

// ─────────────────────── 补翻译队列 ───────────────────────

// 取某句译文:已有直接返回;否则排进串行队列调 translate_sentence(后端落盘)。
// force=false(预翻译)时,排到时文档已切走就跳过,不为旧文档白跑 AI。
function ensureZh(s, opts = {}) {
    if ((s.zh || '').trim()) return Promise.resolve(s.zh);
    if (state.zhInflight.has(s.id)) return state.zhInflight.get(s.id);
    const rel = state.noteRel;
    const p = state.zhChain.then(async () => {
        if ((s.zh || '').trim()) return s.zh;
        if (!opts.force && rel !== state.noteRel) throw new Error('文档已切换');
        const zh = await invoke('translate_sentence', { relPath: rel, sentenceId: s.id });
        s.zh = zh;
        refreshZhUI(s);
        return zh;
    }).finally(() => state.zhInflight.delete(s.id));
    state.zhChain = p.catch(() => {});
    state.zhInflight.set(s.id, p);
    return p;
}

// 译文到了:刷新句行的 .zh;若正是当前播放句,刷新左下角(合成中时由 setSynthing 自带文案)。
function refreshZhUI(s) {
    const row = document.querySelector(`.sentence[data-id="${cssEscape(s.id)}"]`);
    const zhEl = row?.querySelector('.zh');
    if (zhEl) zhEl.textContent = s.zh;
    const cur = state.queue[state.qIndex];
    if (cur && cur.id === s.id && !state.synthing) setNowPlayingFromSentence(s);
}

// 预翻译深度:播放时把当前句 + 后面几句里没译文的排进队列,读到时译文已就绪。
const ZH_PREFETCH_AHEAD = 5;
function prefetchZh() {
    for (let k = 0; k <= ZH_PREFETCH_AHEAD; k++) {
        const s = state.queue[state.qIndex + k];
        if (!s) break;
        if (!s.read_aloud || !(s.en || '').trim()) continue;
        ensureZh(s).catch(() => {});
    }
}

// 开篇预热:打开文档时后台先合成前几句,按下播放即可秒开(每篇只热一次)。
function warmFirstSentences(n = 2) {
    if (!state.noteRel || !state.note) return;
    const readable = allSentences(state.note).filter((s) => s.read_aloud).slice(0, n);
    for (const s of readable) {
        const key = audioKey(s.id);
        if (!state.audioCache.has(key) && !state.audioInflight.has(key)) {
            ensureAudio(s).catch(() => {});
        }
    }
}

function scheduleAdvance(delayMs) {
    const token = ++state.advanceToken;
    setTimeout(() => {
        if (token !== state.advanceToken) return; // 已被新的播放打断
        advance();
    }, delayMs);
}

function advance() {
    if (state.loopMode === 'one') {
        playIndex(state.qIndex);
        return;
    }
    let next = state.qIndex + 1;
    if (next >= state.queue.length) {
        if (state.loopMode === 'all') next = 0;
        else { stopPlayback(); return; }
    }
    playIndex(next);
}

function stopPlayback() {
    state.advanceToken++; // 取消挂起的 advance
    state.playToken++;    // 作废在途合成:回来后 token 不符,不会自播
    state.playing = false;
    if (state.synthing) setSynthing(false);
    audio.pause();
    setPlayBtn(false);
    highlightSentence(null);
    setNowPlaying('—');
}

function togglePlayPause() {
    if (state.synthing) return; // 正在合成:已有反馈(转圈),忽略重复点击,别再起合成
    if (!state.queue.length) {
        if (state.note) playAllDoc();
        return;
    }
    if (audio.paused) {
        if (!audio.src && state.qIndex >= 0) playIndex(state.qIndex);
        else { audio.play(); state.playing = true; setPlayBtn(true); }
    } else {
        audio.pause(); state.playing = false; setPlayBtn(false);
    }
}

function nextSentence() { if (state.queue.length) playIndex(Math.min(state.qIndex + 1, state.queue.length - 1)); }
function prevSentence() { if (state.queue.length) playIndex(Math.max(state.qIndex - 1, 0)); }

function highlightSentence(id) {
    document.querySelectorAll('.sentence.active').forEach((el) => el.classList.remove('active'));
    if (id) {
        const el = document.querySelector(`.sentence[data-id="${cssEscape(id)}"]`);
        if (el) { el.classList.add('active'); el.scrollIntoView({ block: 'center', behavior: 'smooth' }); }
    }
}

// 现在播放区的句子文案(说话人 + 译文/原文),不含尾部计数/状态。
function nowPlayingText(s) {
    const sp = s.speaker ? `<span class="np-speaker">${esc(s.speaker)}</span> ` : '';
    // 朗读时左下角默认显示中文翻译(听英文、看译文对照);没翻译则回退英文原文。
    const text = (s.zh && s.zh.trim()) ? s.zh : s.en;
    return `${sp}${esc(text)}`;
}
function setNowPlayingFromSentence(s) {
    setNowPlaying(`${nowPlayingText(s)}　·　${state.qIndex + 1}/${state.queue.length}`);
}
function setNowPlaying(html) { $('nowPlaying').innerHTML = html; }
function setPlayBtn(playing) { $('playBtn').textContent = playing ? '⏸' : '▶'; }

// ─────────────────────── 掌握状态 ───────────────────────

async function toggleMastered(sentenceId, mastered) {
    try {
        await invoke('set_mastered', { relPath: state.noteRel, sentenceId, mastered });
        const s = findSentence(state.note, sentenceId);
        if (s) s.mastered = mastered;
        const row = document.querySelector(`.sentence[data-id="${cssEscape(sentenceId)}"]`);
        if (row) {
            row.classList.toggle('mastered', mastered);
            const btn = row.querySelector('.s-tool:last-child');
            if (btn) { btn.textContent = mastered ? '已懂 ✓' : '懂了'; btn.classList.toggle('on', mastered); }
        }
    } catch (e) { alert(e); }
}

// ─────────────────────── 导入 ───────────────────────

function bindImport() {
    $('newDocBtn').onclick = newDoc;
    $('emptyNewDocBtn').onclick = newDoc;
    $('convertBtn').onclick = convertNote;
    // 阅读器「编辑原文」:已转化→句块就地编辑;草稿→原文
    $('editRawBtn').onclick = () => { if (!state.noteRel) return; state.forceRaw = false; setViewMode(true); };
    // 编辑器「更多」:展开/收起「回原文重转」(整篇重跑,少用)
    $('editorMoreBtn').onclick = () => { const b = $('toRawBtn'); b.hidden = !b.hidden; };
    // 句块编辑器里「回原文重转」→ 切到原文 textarea
    $('toRawBtn').onclick = () => { state.forceRaw = true; setViewMode(true); };
    // 末尾 AI 追加
    $('appendBtn').onclick = appendViaAi;
    // 编辑器底部「返回阅读」
    $('editorBackBtn').onclick = () => setViewMode(false);
    // 自动保存(防抖):原文 textarea + 标题(两态共用)
    $('editRaw').addEventListener('input', scheduleSave);
    $('editTitle').addEventListener('input', scheduleSave);
}

// 新建文档(草稿,只有原文,未转化)—— 立刻建 + 打开编辑器 + 聚焦正文。
// folder 指定放到哪个目录('' = 根)。
async function newDocIn(folder) {
    if (!state.config?.vault_path) { alert('请先在设置里选择库目录'); openSettings(); return; }
    try {
        const note = await invoke('create_draft', { title: '', folder: folder || '', raw: '' });
        await refreshTree();
        const rel = findRel(note.id);
        if (rel) await openNote(rel, { forceEditor: true });
    } catch (e) { alert('新建失败:' + e); }
}
function newDoc() { newDocIn(''); } // 顶栏「＋新建文档」→ 建在根

// 自动保存(防抖)。句块模式→save_blocks(+标题 save_raw);原文模式→save_raw。
let saveTimer = null;
let lastSavedTitle = null;
function scheduleSave() {
    if (!state.noteRel) return;
    const s = state.structMode ? $('editorStatus') : $('editorStatus');
    s.className = 'field-note'; s.textContent = '编辑中…';
    clearTimeout(saveTimer);
    saveTimer = setTimeout(doSave, 700);
}
async function doSave() {
    if (!state.noteRel) return;
    const title = $('editTitle').value.trim();
    if (state.structMode) {
        try {
            const audioChanged = await invoke('save_blocks', { relPath: state.noteRel, blocks: JSON.parse(JSON.stringify(state.editBlocks)) });
            if (title && title !== (state.note?.title || '')) {
                const newRel = await invoke('save_raw', { relPath: state.noteRel, raw: state.note?.raw || '', newTitle: title });
                if (newRel && newRel !== state.noteRel) { state.noteRel = newRel; refreshTree(); }
            }
            if (state.note) { state.note.blocks = JSON.parse(JSON.stringify(state.editBlocks)); state.note.title = title || state.note.title; }
            setStructStatus(audioChanged ? '✓ 已保存。改过的句音频已失效,听读前点「生成音频」重生。' : '✓ 已自动保存', audioChanged ? 'ok' : 'ok');
        } catch (e) { setStructStatus('✗ 保存失败:' + e, 'err'); }
    } else {
        try {
            const raw = $('editRaw').value;
            const newRel = await invoke('save_raw', { relPath: state.noteRel, raw, newTitle: title || null });
            const titleChanged = title && title !== lastSavedTitle;
            if (newRel && newRel !== state.noteRel) { state.noteRel = newRel; lastSavedTitle = title; }
            else if (titleChanged) { lastSavedTitle = title; }
            if (state.note) { state.note.raw = raw; state.note.title = title || state.note.title; }
            const s = $('editorStatus'); s.className = 'field-note ok'; s.textContent = '✓ 已自动保存';
            if (titleChanged) refreshTree();
        } catch (e) { const s = $('editorStatus'); s.className = 'field-note err'; s.textContent = '✗ 保存失败:' + e; }
    }
}

// AI 转化原文 → 句块(先存原文,再转化)。可反复:改完原文重新转化。
async function convertNote() {
    if (!state.config?.has_api_key) { alert('请先在设置里配置 AI 模型与 Key'); openSettings(); return; }
    const status = $('editorStatus');
    status.className = 'field-note';
    status.textContent = '保存原文并 AI 转化中(清理 → 保结构 → 切句 → 翻译)…';
    $('convertBtn').disabled = true;
    try {
        await invoke('save_raw', { relPath: state.noteRel, raw: $('editRaw').value, newTitle: $('editTitle').value.trim() || null });
        const note = await invoke('convert_note', { relPath: state.noteRel });
        state.note = note;
        await refreshTree();
        const newRel = findRel(note.id) || state.noteRel;
        state.noteRel = newRel;
        state.forceRaw = false; // 转化完成,离开原文模式
        setActiveNote(newRel);
        setViewMode(false); // 进阅读器(家)
        generateAudioAuto(newRel); // 后台自动生成音频,直达听读
    } catch (e) {
        status.className = 'field-note err';
        status.textContent = '✗ 转化失败:' + e;
    } finally {
        $('convertBtn').disabled = false;
    }
}

// 末尾 AI 追加:只对新贴的原文跑 AI(清理→切句→翻译),接到文末。
// 前面已有的句块 / 音频完全不动(后端 append_via_ai 保证)。
async function appendViaAi() {
    if (!state.noteRel) return;
    if (!state.config?.has_api_key) { alert('请先在设置里配置 AI 模型与 Key'); openSettings(); return; }
    const raw = $('appendRaw').value.trim();
    if (!raw) { $('appendRaw').focus(); return; }
    const status = $('appendStatus');
    status.className = 'field-note'; status.textContent = 'AI 追加中(清理 → 切句 → 翻译)…';
    $('appendBtn').disabled = true;
    try {
        const note = await invoke('append_via_ai', { relPath: state.noteRel, raw });
        state.note = note;
        $('appendRaw').value = '';
        fillStructEditor();          // 重载 editBlocks + 重渲染(含新句)
        status.className = 'field-note ok'; status.textContent = '✓ 已追加到文末,可直接听读';
        refreshTree();
    } catch (e) {
        status.className = 'field-note err'; status.textContent = '✗ 追加失败:' + e;
    } finally {
        $('appendBtn').disabled = false;
    }
}

async function generateAudio(rel) {
    if (!rel) return;
    if (!(await confirmDialog('把整篇按当前音色预缓存到本地?(用于离线;已存在的句子跳过)', { okText: '预缓存' }))) return;
    setNowPlaying('预缓存当前音色中…');
    try {
        const res = await invokeGenerateAudio(rel, (done, total) => {
            setNowPlaying(total ? `预缓存当前音色中… ${done}/${total} 句` : '预缓存当前音色中…');
        });
        await openNote(rel); // 重新加载,刷新 audio_ready
        if (res.missing > 0) {
            setNowPlaying(`已缓存 ${res.generated} 句,还差 ${res.missing} 句(可再点重试补齐)。`);
        } else {
            setNowPlaying(`当前音色已缓存:${res.generated} 句,跳过 ${res.skipped} 句。`);
        }
    } catch (e) {
        alert('预缓存失败:' + e);
        setNowPlaying('—');
    }
}

// 转化后自动按当前音色预缓存(不弹确认),完成后进可听读状态。
// 失败也无妨:实时合成会在播放时按需补,不阻塞听读。
async function generateAudioAuto(rel) {
    if (!rel) return;
    setNowPlaying('正在预缓存逐句音频…(也可直接点句子开始听,边听边合成)');
    try {
        await invokeGenerateAudio(rel, (done, total) => {
            setNowPlaying(total ? `正在预缓存逐句音频… ${done}/${total}(也可直接点句子边听边合成)` : '正在预缓存逐句音频…(也可直接点句子边听边合成)');
        });
        await openNote(rel); // 刷新 audio_ready + genAudioBtn
        setNowPlaying('音频就绪,点句子或 ▶ 开始听。');
    } catch (e) {
        setNowPlaying('预缓存未完成:' + e + '(不影响,可直接播放按需合成)');
    }
}

// 播放页「读名字」热开关:读/不读名字是两个缓存维度(nospk 目录),切换即时生效,无需重生成。
// 也把新状态存进 config 作为下次默认。
async function toggleReadSpeaker(on) {
    state.readSpeaker = on;
    $('ttsReadSpeaker').checked = on;       // 同步设置页那个开关
    $('playReadSpeakerChk').checked = on;   // 同步播放栏开关(可能由设置页触发)
    if (state.config?.tts) state.config.tts.read_speaker = on;
    invoke('set_tts_read_speaker', { on }).catch(() => {}); // 持久化默认,失败不阻塞热切换
    reapplyPlayback(on ? '切到读名字,合成中…' : '切到只读正文,合成中…');
}

// ─────────────────────── 设置 ───────────────────────

function bindSettings() {
    $('settingsBtn').onclick = openSettings;
    $('backToReaderBtn').onclick = closeSettings;
    document.querySelectorAll('.settings-tab').forEach((tab) => {
        tab.onclick = () => switchSettingsTab(tab.dataset.settingsTab);
    });
    $('pickVaultBtn').onclick = async () => {
        const picked = await invoke('pick_directory');
        if (picked) $('vaultPath').value = picked;
    };
    $('providerSel').onchange = changeProvider;
    $('ttsProviderSel').onchange = () => setTtsProviderUI($('ttsProviderSel').value);
    $('ttsVoiceAddBtn').onclick = addTtsVoice;
    $('ttsVoiceDelBtn').onclick = delTtsVoice;
    $('ttsScanVoicesBtn').onclick = scanSystemVoices;
    $('testTtsBtn').onclick = testTts;
    $('fetchModelsBtn').onclick = fetchModels;
    $('openDataDirBtn').onclick = openDataDirectory;
    $('saveSettingsBtn').onclick = saveSettings;
    $('openVaultBtn').onclick = () => invoke('open_vault').catch(alert);
    $('refreshBtn').onclick = refreshTree;
    $('newFolderBtn').onclick = newFolder;
    $('newFolderInput').addEventListener('keydown', (e) => {
        if (e.key === 'Enter') { e.preventDefault(); doCreateFolder(); }
        else if (e.key === 'Escape') { e.target.hidden = true; e.target.value = ''; }
    });
    $('newFolderInput').addEventListener('blur', (e) => {
        if (!e.target.value.trim()) { e.target.hidden = true; }
    });
    $('checkUpdateBtn').onclick = checkUpdate;
}

// 模型下拉:保证当前已保存的模型始终在里面,拉取后追加更多。
function setModelOptions(ids, selected) {
    const sel = $('modelInput');
    const all = [...new Set(ids.filter(Boolean))];
    if (selected && !all.includes(selected)) all.unshift(selected);
    sel.innerHTML = all.map((id) => `<option value="${esc(id)}">${esc(id)}</option>`).join('');
    if (selected) sel.value = selected;
}

function changeProvider() {
    const base = providerDefault($('providerSel').value);
    if (base) $('baseUrlInput').value = base;
    $('modelStatus').textContent = '';
}

async function fetchModels() {
    const btn = $('fetchModelsBtn');
    btn.disabled = true;
    $('modelStatus').textContent = '正在获取模型列表…';
    try {
        const models = await invoke('fetch_models', {
            provider: $('providerSel').value,
            baseUrl: $('baseUrlInput').value.trim(),
            apiKey: $('apiKeyInput').value || null,
        });
        const current = $('modelInput').value;
        setModelOptions(models.map((m) => m.id), current || models[0]?.id);
        $('modelStatus').textContent = `已获取 ${models.length} 个模型`;
    } catch (e) {
        $('modelStatus').textContent = `获取失败：${String(e)}`;
    } finally {
        btn.disabled = false;
    }
}

async function openDataDirectory() {
    try { await invoke('open_data_directory'); } catch (e) { alert(e); }
}

function openSettings() {
    document.querySelector('.body').hidden = true;
    $('transport').hidden = true;
    $('settingsView').hidden = false;
    switchSettingsTab('basic');
    loadAbout();
}

function closeSettings() {
    $('settingsView').hidden = true;
    document.querySelector('.body').hidden = false;
    $('transport').hidden = !state.note;
}

function switchSettingsTab(name) {
    document.querySelectorAll('.settings-tab').forEach((b) => b.classList.toggle('active', b.dataset.settingsTab === name));
    document.querySelectorAll('.settings-panel').forEach((p) => p.classList.toggle('active', p.dataset.settingsPanel === name));
}

async function loadAbout() {
    try {
        const version = await invoke('get_app_version');
        $('aboutVersion').textContent = `v${version}`;
    } catch (_) {
        $('aboutVersion').textContent = '未知';
    }
    try {
        const sys = await invoke('get_system_info');
        $('aboutBuildType').textContent = sys.build_type;
        $('aboutPlatform').textContent = `${sys.platform} (${sys.arch})`;
        $('aboutOsVersion').textContent = sys.os_version;
    } catch (_) { /* 忽略 */ }
}

// 关于/更新:照搬自 DirDetective 的实现(动态 import updater,带进度的下载安装)。
async function checkUpdate() {
    const checkBtn = $('checkUpdateBtn');
    const dlBtn = $('downloadUpdateBtn');
    const status = $('updateStatus');
    const latest = $('latestVersion');
    try {
        checkBtn.disabled = true;
        checkBtn.textContent = '检查中…';
        status.textContent = '正在检查…';
        latest.textContent = '检查中…';
        dlBtn.hidden = true;

        const { check } = await import('@tauri-apps/plugin-updater');
        const { getVersion } = await import('@tauri-apps/api/app');
        const currentVersion = await getVersion();
        const update = await check({ timeout: 30000 });

        if (!update) {
            status.textContent = '当前版本已是最新';
            latest.textContent = `v${currentVersion} (最新)`;
            return;
        }

        latest.textContent = `v${update.version}`;
        status.textContent = `发现新版本 v${update.version}`;
        dlBtn.hidden = false;
        dlBtn.textContent = '下载并安装';
        dlBtn.onclick = async () => {
            try {
                dlBtn.disabled = true;
                dlBtn.textContent = '下载中…';
                status.textContent = '正在下载更新…';
                let downloaded = 0;
                await update.downloadAndInstall((event) => {
                    if (event.event === 'Progress') {
                        downloaded += event.data.chunkLength;
                        status.textContent = `下载中: ${formatSize(downloaded)}`;
                    }
                });
                status.textContent = '安装完成，请手动重启应用';
                dlBtn.textContent = '安装完成';
            } catch (error) {
                status.textContent = `更新失败：${String(error)}`;
                dlBtn.disabled = false;
                dlBtn.textContent = '重试';
            }
        };
    } catch (error) {
        status.textContent = `检查失败：${String(error)}`;
        latest.textContent = '检查失败';
        dlBtn.hidden = true;
    } finally {
        checkBtn.disabled = false;
        checkBtn.textContent = '检查更新';
    }
}

function formatSize(bytes) {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let value = Number(bytes) || 0;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
        value /= 1024;
        unit += 1;
    }
    return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function providerDefault(p) {
    return { zhipu: 'https://open.bigmodel.cn/api/paas/v4', deepseek: 'https://api.deepseek.com',
        openai: 'https://api.openai.com/v1', openrouter: 'https://openrouter.ai/api/v1' }[p] || '';
}

// 把当前表单落盘(不关设置页),返回最新 PublicConfig。保存按钮和试听都用它。
async function persistSettings() {
    const input = {
        provider: $('providerSel').value,
        base_url: $('baseUrlInput').value.trim(),
        model: $('modelInput').value.trim(),
        vault_path: $('vaultPath').value.trim(),
        tts: {
            provider: $('ttsProviderSel').value,
            voice: $('ttsVoice').value.trim(),
            rate: $('ttsRate').value.trim(),
            read_speaker: $('ttsReadSpeaker').checked,
            voices: ttsVoices,
            credentials: {
                zhipu: { api_key: $('ttsZhipuKey').value },
                doubao: {
                    api_key: $('ttsDoubaoKey').value,
                    resource_id: $('ttsDoubaoResource').value.trim(),
                },
                aliyun: {
                    app_key: $('ttsAliyunAppKey').value.trim(),
                    access_key_id: $('ttsAliyunAkId').value.trim(),
                    access_key_secret: $('ttsAliyunAkSecret').value,
                    region: $('ttsAliyunRegion').value.trim(),
                },
            },
        },
    };
    const key = $('apiKeyInput').value;
    if (key.trim()) input.api_key = key.trim();
    const cfg = await invoke('save_config_command', { input });
    state.config = cfg;
    applyConfigToUI(cfg);
    await refreshTree();
    return cfg;
}

// 保存后留在设置页(不自动返回),按钮短暂提示「已保存」。要返回点「← 返回」。
async function saveSettings() {
    const btn = $('saveSettingsBtn');
    try {
        await persistSettings();
        const orig = btn.textContent;
        btn.textContent = '已保存 ✓';
        btn.disabled = true;
        setTimeout(() => { btn.textContent = orig; btn.disabled = false; }, 1200);
    } catch (e) { alert(e); }
}

// 试听:先把当前厂商/音色/凭证落盘(凭证必须保存才能用),再合成一句样本播放。
// 出错把后端真实错误内联红字显示,不弹窗、不清,方便排查。
async function testTts() {
    const btn = $('testTtsBtn');
    const status = $('ttsTestStatus');
    btn.disabled = true;
    btn.textContent = '合成中…';
    status.className = 'field-note';
    status.textContent = '保存并合成样本…';
    try {
        await persistSettings();
        const url = await invoke('test_tts');
        status.className = 'field-note ok';
        status.textContent = '▶ 播放中…';
        const a = new Audio(url);
        a.onended = () => { status.textContent = '✓ 试听完成'; };
        a.onerror = () => { status.className = 'field-note err'; status.textContent = '✗ 播放失败(音频无法解码)'; };
        await a.play();
    } catch (e) {
        status.className = 'field-note err';
        status.textContent = '✗ ' + String(e).replace(/^试听失败:\s*/, '');
    } finally {
        btn.disabled = false;
        btn.textContent = '▶ 试听';
    }
}

// ─────────────────────── 即查 ───────────────────────

function bindWordPopover() {
    $('wordClose').onclick = hideWordPopover;
    document.addEventListener('click', (e) => {
        const pop = $('wordPopover');
        if (!pop.hidden && !pop.contains(e.target) && !e.target.classList.contains('word')) {
            hideWordPopover();
        }
    });
}

async function showWordPopover(wordEl, sentence) {
    const word = wordEl.dataset.word;
    const pop = $('wordPopover');
    $('wordTitle').textContent = word;
    $('wordIpa').textContent = '';
    $('wordBody').textContent = '查询中…';
    $('wordSpeak').onclick = (e) => { e.stopPropagation(); speakWord(word); };
    pop.hidden = false;
    // 定位
    const r = wordEl.getBoundingClientRect();
    const pw = 320;
    let left = r.left;
    if (left + pw > window.innerWidth - 12) left = window.innerWidth - pw - 12;
    pop.style.left = Math.max(12, left) + 'px';
    pop.style.top = (r.bottom + 8) + 'px';
    try {
        const ans = await invoke('lookup_word', { word, context: sentence.en });
        if ($('wordTitle').textContent !== word) return; // 期间已点了别的词
        // 首行是音标(/…/ 或 […])→ 放到标题旁,正文去掉这行
        const lines = ans.trim().split('\n');
        const first = (lines[0] || '').trim();
        if (/^[\/\[].+[\/\]]$/.test(first)) {
            $('wordIpa').textContent = first;
            lines.shift();
        }
        $('wordBody').textContent = lines.join('\n').trim();
    } catch (e) {
        $('wordBody').textContent = '查询失败:' + e;
    }
}

// 即查发音:后端系统 say 直接外放;macos 厂商时用当前正文音色。发音期间按钮高亮,忽略重复点击。
async function speakWord(word) {
    const btn = $('wordSpeak');
    if (btn.classList.contains('speaking')) return;
    btn.classList.add('speaking');
    try {
        await invoke('speak_word', { word, voice: state.currentVoice || state.config?.tts?.voice || null });
    } catch (e) {
        $('wordBody').textContent += '\n(' + e + ')';
    } finally {
        btn.classList.remove('speaking');
    }
}
function hideWordPopover() { $('wordPopover').hidden = true; }

// ─────────────────────── 传输栏 ───────────────────────

function bindTransport() {
    $('playBtn').onclick = togglePlayPause;
    $('prevBtn').onclick = prevSentence;
    $('nextBtn').onclick = nextSentence;
    $('playAllBtn').onclick = playAllDoc;
    $('transportMoreBtn').onclick = () => { const e = $('transportExtra'); e.hidden = !e.hidden; };
    $('playReadSpeakerChk').addEventListener('change', (e) => toggleReadSpeaker(e.target.checked));
    $('revealNoteBtn').onclick = () => state.noteRel && invoke('reveal_path', { relPath: state.noteRel }).catch(alert);
    $('masteredBtn').onclick = () => {
        const s = state.queue[state.qIndex];
        if (s) toggleMastered(s.id, true).then(() => { if (state.skipMastered) advance(); });
    };

    $('loopMode').querySelectorAll('button').forEach((b) => {
        b.onclick = () => {
            state.loopMode = b.dataset.mode;
            $('loopMode').querySelectorAll('button').forEach((x) => x.classList.toggle('active', x === b));
        };
    });
    $('skipMasteredChk').onchange = (e) => state.skipMastered = e.target.checked;
    $('voiceSel').onchange = (e) => switchVoice(e.target.value);
    $('rateSel').onchange = (e) => { state.speed = Number(e.target.value); if (audio.src) audio.playbackRate = state.speed; };
    $('gapRange').oninput = (e) => { state.gapMs = Number(e.target.value); $('gapVal').textContent = (state.gapMs / 1000).toFixed(1) + 's'; };
    $('showZhChk').onchange = (e) => {
        state.showZh = e.target.checked;
        document.querySelectorAll('.sentence').forEach((el) => el.classList.toggle('show-zh', state.showZh));
    };

    audio.addEventListener('ended', () => scheduleAdvance(state.gapMs));
    audio.addEventListener('pause', () => { /* 由用户操作触发,不自动改 playing 状态 */ });
}

// ─────────────────────── 窗口拖动 ───────────────────────

// 用 start_window_drag 命令(mousedown 触发),比 CSS region / data-tauri-drag-region 在
// 透明窗口 + Overlay 标题栏下更可靠。按钮等交互控件不触发。
function bindDrag() {
    document.querySelectorAll('.titlebar').forEach((el) => {
        el.addEventListener('mousedown', async (e) => {
            if (e.button !== 0) return;
            if (e.target.closest('button, input, select, a, [data-nodrag]')) return;
            try { await invoke('start_window_drag'); } catch (_) {}
        });
    });
}

// ─────────────────────── 弹窗通用 ───────────────────────

function bindGlobal() {
    document.querySelectorAll('.modal').forEach((m) => {
        m.querySelectorAll('[data-close]').forEach((b) => b.addEventListener('click', () => closeModal(m.id)));
    });
    document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape') document.querySelectorAll('.modal.open').forEach((m) => closeModal(m.id));
    });
}
function openModal(id) { $(id).classList.add('open'); $(id).setAttribute('aria-hidden', 'false'); }
function closeModal(id) { $(id).classList.remove('open'); $(id).setAttribute('aria-hidden', 'true'); }

// ─────────────────────── 小工具 ───────────────────────

function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
}
function esc(s) {
    return String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}
function cssEscape(s) { return (window.CSS?.escape?.(s)) ?? String(s).replace(/"/g, '\\"'); }

init().catch((e) => console.error('init failed', e));
