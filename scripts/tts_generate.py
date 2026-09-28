#!/usr/bin/env python3
"""Attune —— 句级 mp3 预处理脚本(多厂商 TTS)。

读一篇文档 JSON + 应用 config.json,按 config 里的 tts_provider 分发:
  - edge   : edge-tts(微软,免费,无需凭证;需 pip install edge-tts)
  - zhipu  : 智谱 GLM-TTS(POST /api/paas/v4/audio/speech,复用智谱 API Key)
  - doubao : 火山引擎(豆包)TTS(POST openspeech.bytedance.com/api/v1/tts,需 appid/token/cluster)
  (macOS 系统音色 tts_provider=macos 由应用内 Rust 直连 say 合成,不经此脚本)

为每个 read_aloud 的句生成一个 mp3,输出到 media/{note.id}/{音色}/序号.ext。
按「文档 + 音色」分层:换音色 = 换子目录,互不覆盖,听读中途可随时切;每个音色各自缓存。
read_aloud:false 的句跳过。已存在且非空的文件跳过(断点续跑 / 秒回放)。

用法:
    python3 tts_generate.py --note <abs_note.json> --config <app_config.json>
"""

import argparse
import base64
import json
import sys
import urllib.request
import urllib.error
import uuid
from pathlib import Path

ZHIPU_TTS_URL = "https://open.bigmodel.cn/api/paas/v4/audio/speech"
DOUBAO_TTS_URL = "https://openspeech.bytedance.com/api/v3/tts/unidirectional"


def voice_key(voice):
    """把音色 id 清洗成文件系统安全的目录名(须与 Rust sanitize_voice 规则一致)。
    保留 ASCII 字母数字与 - _,其余一律换成 _;空则回退 'default'。"""
    v = (voice or "").strip()
    key = "".join(c if ((c.isascii() and c.isalnum()) or c in "-_") else "_" for c in v)
    return key or "default"


def cache_folder(voice, read_speaker):
    """缓存子目录名(须与 Rust cache_folder 一致):读名字=纯音色名(默认,现有文件即此);
    不读名字=音色名 + '-nospk'。使「读名字」成为热开关的一个缓存维度。"""
    return voice_key(voice) + ("" if read_speaker else "-nospk")


def iter_sentences(blocks):
    for block in blocks:
        btype = block.get("type")
        if btype == "paragraph":
            for s in block.get("sentences", []):
                yield s
        elif btype == "list":
            for item in block.get("items", []):
                for s in item.get("sentences", []):
                    yield s


def tts_text(sentence, read_speaker=True):
    speaker = (sentence.get("speaker") or "").strip()
    en = (sentence.get("en") or "").strip()
    if not en:
        return ""
    if read_speaker and speaker:
        return f"{speaker}. {en}"
    return en


def parse_rate(rate):
    """把 "-8%" 或 "0.92" 解析成倍速 float(给 doubao/zhipu 用;edge 直接用字符串)。"""
    rate = (rate or "").strip()
    if not rate:
        return 1.0
    try:
        if rate.endswith("%"):
            return max(0.5, min(2.0, 1.0 + float(rate[:-1]) / 100.0))
        return max(0.5, min(2.0, float(rate)))
    except ValueError:
        return 1.0


# ─────────────────────── 各厂商合成 ───────────────────────

def synth_edge(text, voice, rate, out_path):
    import asyncio
    import edge_tts  # 延迟导入,只在用 edge 时才需要
    communicate = edge_tts.Communicate(text, voice=voice, rate=rate)
    # Communicate.save 是协程,主流程是同步的,用 asyncio.run 驱动它
    asyncio.run(communicate.save(str(out_path)))


def _http_post_json(url, headers, body_dict):
    data = json.dumps(body_dict).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers=headers, method="POST")
    with urllib.request.urlopen(req, timeout=60) as resp:
        return resp.status, dict(resp.headers), resp.read()


def synth_zhipu(text, voice, rate, api_key, out_path):
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
    }
    body = {
        "model": "glm-tts",
        "input": text,
        "voice": voice or "female",
        # 智谱 GLM-TTS 仅支持 wav / pcm,不支持 mp3(会报错 1214)
        "response_format": "wav",
        "speed": parse_rate(rate),
    }
    status, headers, raw = _http_post_json(ZHIPU_TTS_URL, headers, body)
    ctype = headers.get("Content-Type", "")
    if status != 200 or not ctype.startswith("audio"):
        try:
            err = json.loads(raw.decode("utf-8"))
            raise RuntimeError(f"智谱 TTS 返回错误: {err}")
        except (UnicodeDecodeError, ValueError):
            raise RuntimeError(f"智谱 TTS 返回非音频(HTTP {status}, {ctype})")
    out_path.write_bytes(raw)


def doubao_speech_rate(rate):
    """V3 speech_rate:[-50,100],0=1.0x,100=2.0x,-50=0.5x。把 "-8%" 或 0.92 映射进去。"""
    r = (rate or "").strip()
    try:
        pct = float(r[:-1]) if r.endswith("%") else (float(r) - 1.0) * 100.0
    except ValueError:
        return 0
    return max(-50, min(100, int(round(pct))))


def synth_doubao(text, voice, rate, api_key, resource_id, out_path):
    # 豆包语音合成大模型 V3 单向流式 HTTP。
    # 端点 /api/v3/tts/unidirectional,鉴权用 X-Api-Key 头(不是 Bearer)。
    # 响应是流式多个 JSON 对象(换行分隔),每块 data 为 base64 音频,需拼接。
    headers = {
        "Content-Type": "application/json",
        "X-Api-Key": api_key,
        "X-Api-Resource-Id": resource_id or "seed-tts-2.0",
        "X-Api-Request-Id": str(uuid.uuid4()),
    }
    body = json.dumps({
        "req_params": {
            "text": text,
            "speaker": voice,
            "audio_params": {
                "format": "mp3",
                "speech_rate": doubao_speech_rate(rate),
            },
        }
    }).encode("utf-8")
    req = urllib.request.Request(DOUBAO_TTS_URL, data=body, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read()
    except urllib.error.HTTPError as e:
        detail = e.read().decode("utf-8", "replace")[:300]
        raise RuntimeError(f"豆包 TTS HTTP {e.code}: {detail}")
    # 流式响应:逐行解析 JSON,拼 data;错误码可能在顶层或 header 里
    chunks = []
    err = None
    for line in raw.decode("utf-8", "replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except ValueError:
            continue
        if obj.get("data"):
            chunks.append(base64.b64decode(obj["data"]))
        hdr = obj.get("header") or {}
        code = obj.get("code", hdr.get("code"))
        msg = obj.get("message", hdr.get("message"))
        if code not in (None, 0, 3000) and msg:
            err = f"code={code}: {msg}"
    if not chunks:
        raise RuntimeError(f"豆包 TTS 未返回音频{('，' + err) if err else ''}")
    out_path.write_bytes(b"".join(chunks))


# 阿里云 NLS 语速:-500~500(0=正常)。把 "-8%" 或 0.92 映射进去。
def aliyun_speech_rate(rate):
    r = (rate or "").strip()
    try:
        pct = float(r[:-1]) if r.endswith("%") else (float(r) - 1.0) * 100.0
    except ValueError:
        return 0
    return max(-500, min(500, int(round(pct * 5))))


def synth_aliyun(text, voice, rate, app_key, access_key_id, access_key_secret, region, out_path):
    # 依赖:1) nls 阿里云 NLS Python SDK;2) alibabacloud_nls_cloud_meta 换 token。
    #   pip install nls alibabacloud_nls_cloud_meta20190228 alibabacloud_tea_openapi
    #   (nls 官方在 GitHub:https://github.com/aliyun/alibabacloud-nls-python-sdk)
    try:
        import nls
        from alibabacloud_nls_cloud_meta20190228.client import Client as MetaClient
        from alibabacloud_nls_cloud_meta20190228 import models as meta_models
        from alibabacloud_tea_openapi import models as openapi_models
    except ImportError as e:
        raise RuntimeError(
            "未安装阿里云 NLS SDK。请装:pip install nls alibabacloud_nls_cloud_meta20190228 alibabacloud_tea_openapi"
            f"（{e}）"
        )

    # 1) 用 AccessKey 换 NLS token
    cfg = openapi_models.Config(access_key_id=access_key_id, access_key_secret=access_key_secret)
    cfg.endpoint = f"nls-meta.{region}.aliyuncs.com"
    meta = MetaClient(cfg)
    body = meta.create_token(meta_models.CreateTokenRequest()).body
    token = None
    # 不同 SDK 版本字段名不同,防御性取值
    for getter in (
        lambda: body.token.nls_token,
        lambda: body.token_nlg,
        lambda: body.token,
    ):
        try:
            v = getter()
            if v:
                token = v
                break
        except Exception:
            continue
    if not token:
        raise RuntimeError("无法从 CreateToken 响应取到 token(阿里云 SDK 版本字段名可能不同)")

    # 2) WebSocket 合成
    url = f"wss://nls-gateway-{region}.aliyuncs.com/ws/v1"
    chunks = []
    err = {"msg": None}

    def _on_data(data, *a):
        chunks.append(data)

    def _on_error(message, *a):
        err["msg"] = message

    synth_obj = nls.NlsSpeechSynthesizer(
        url=url,
        token=token,
        appkey=app_key,
        aformat="mp3",
        on_data=_on_data,
        on_error=_on_error,
        on_completed=lambda *a: None,
        on_close=lambda *a: None,
    )
    synth_obj.start(text, voice=voice or "xiaoyun", speech_rate=aliyun_speech_rate(rate))
    if err["msg"]:
        raise RuntimeError(f"阿里云 TTS 错误: {err['msg']}")
    if not chunks:
        raise RuntimeError("阿里云 TTS 未返回音频")
    out_path.write_bytes(b"".join(chunks))


# ─────────────────────── 主流程 ───────────────────────

def main():
    parser = argparse.ArgumentParser(description="Attune 句级 mp3 预处理(多厂商 TTS)")
    parser.add_argument("--note", help="文档 JSON 的绝对路径(非 --test 时必填)")
    parser.add_argument("--config", required=True, help="应用 config.json 的路径")
    parser.add_argument("--test", action="store_true", help="试听模式:合成一句样本,stdout 打印输出文件绝对路径")
    parser.add_argument("--only", help="只合成指定 id 的单句(按需补句,忽略已存在检查强制重生成)")
    parser.add_argument("--voice", help="覆盖 config 里的音色(实时切换音色时用;不传则用 config 的)")
    parser.add_argument("--read-speaker", dest="read_speaker", choices=["0", "1"],
                        help="覆盖是否朗读说话人姓名(热开关:1=读名字,0=只读正文;不传则用 config 的)")
    args = parser.parse_args()

    if not args.test and not args.note:
        parser.error("--note 必填(除非 --test)")

    config_path = Path(args.config).expanduser().resolve()
    config = json.loads(config_path.read_text(encoding="utf-8")) if config_path.is_file() else {}

    tts = config.get("tts") or {}
    provider = (tts.get("provider") or "edge").strip()
    # 音色优先用命令行覆盖(实时切换),否则用 config 里选的。
    voice = ((args.voice or "").strip()) or (tts.get("voice") or "").strip()
    rate = tts.get("rate", "-8%")
    # 是否读名字:命令行覆盖(热开关)优先,否则用 config 的。
    read_speaker = (args.read_speaker == "1") if args.read_speaker is not None \
        else tts.get("read_speaker", True)
    creds = tts.get("credentials") or {}

    if provider == "edge":
        try:
            import edge_tts  # noqa: F401
        except ImportError:
            sys.stderr.write("缺少 edge-tts:pip install --index-url https://pypi.org/simple/ edge-tts\n")
            sys.exit(2)
        if not voice:
            voice = "en-US-AriaNeural"
        print(f"[edge-tts] voice={voice} rate={rate}", flush=True)
        synth = lambda text, out: synth_edge(text, voice, rate, out)
    elif provider == "zhipu":
        api_key = ((creds.get("zhipu") or {}).get("api_key") or "").strip()
        if not api_key:
            sys.stderr.write("智谱 TTS 缺少 API Key(请在设置里单独填写智谱 TTS API Key)。\n")
            sys.exit(2)
        if not voice:
            voice = "female"
        print(f"[智谱 GLM-TTS] voice={voice} speed={parse_rate(rate)}", flush=True)
        synth = lambda text, out: synth_zhipu(text, voice, rate, api_key, out)
    elif provider == "doubao":
        d = creds.get("doubao") or {}
        api_key = (d.get("api_key") or "").strip()
        if not api_key:
            # 兼容旧 V1 结构:appid 形如 api-key-/sk- 时当 API Key,否则取 token
            appid = (d.get("appid") or "").strip()
            token = (d.get("token") or "").strip()
            api_key = appid if (appid.startswith("api-key") or appid.startswith("sk-")) else token
        resource_id = (d.get("resource_id") or "").strip() or "seed-tts-2.0"
        if not api_key:
            sys.stderr.write("豆包 TTS 缺少 API Key(在设置里填控制台的 API Key,api-key- 开头)。\n")
            sys.exit(2)
        if not voice:
            voice = "BV700_streaming"
        print(f"[火山豆包 TTS V3] speaker={voice} resource={resource_id} speech_rate={doubao_speech_rate(rate)}", flush=True)
        synth = lambda text, out: synth_doubao(text, voice, rate, api_key, resource_id, out)
    elif provider == "aliyun":
        a = creds.get("aliyun") or {}
        app_key = (a.get("app_key") or "").strip()
        ak_id = (a.get("access_key_id") or "").strip()
        ak_sec = (a.get("access_key_secret") or "").strip()
        region = (a.get("region") or "cn-shanghai").strip() or "cn-shanghai"
        if not (app_key and ak_id and ak_sec):
            sys.stderr.write("阿里云 TTS 缺少凭证(在设置里填 AppKey / AccessKey ID / AccessKey Secret)。\n")
            sys.exit(2)
        if not voice:
            voice = "xiaoyun"
        print(f"[阿里云 NLS] voice={voice} region={region} speech_rate={aliyun_speech_rate(rate)}", flush=True)
        synth = lambda text, out: synth_aliyun(text, voice, rate, app_key, ak_id, ak_sec, region, out)
    else:
        if provider == "macos":
            sys.stderr.write("macOS 系统音色由应用内 Rust 直连 say 合成,不经此脚本。\n")
        else:
            sys.stderr.write(f"未知 TTS 厂商: {provider}\n")
        sys.exit(2)

    # 智谱 GLM-TTS 只出 wav;edge / 豆包 出 mp3
    out_ext = "wav" if provider == "zhipu" else "mp3"

    # ── 试听模式:合成一句样本,打印输出路径给 Rust 读取 ──
    if args.test:
        import tempfile
        sample = "Hello. This is a quick voice test. One, two, three."
        out = Path(tempfile.gettempdir()) / f"attune_tts_test.{out_ext}"
        try:
            synth(sample, out)
            print(str(out.resolve()), flush=True)
            sys.exit(0)
        except Exception as e:
            sys.stderr.write(f"试听合成失败: {e}\n")
            sys.exit(1)

    note_path = Path(args.note).expanduser().resolve()
    if not note_path.is_file():
        sys.stderr.write(f"找不到文档: {note_path}\n")
        sys.exit(2)

    note = json.loads(note_path.read_text(encoding="utf-8"))
    # 每篇文档 + 每个音色各占一个子目录:media/{note.id}/{音色}/序号.ext。
    # 按音色分层 → 听读中途可随时切音色:每个音色第一遍听时缓存,之后离线秒回放。
    note_id = (note.get("id") or note_path.stem).strip()
    vkey = cache_folder(voice, read_speaker)
    # 音频统一挂在 vault 根的 media/(按 note.id 索引,和文档所在子文件夹解耦);
    # 拿不到 vault 路径时才退回文档同级目录。
    vault = (config.get("vault_path") or "").strip()
    media_root = (Path(vault) if vault else note_path.parent) / "media"
    media_dir = media_root / note_id / vkey
    media_dir.mkdir(parents=True, exist_ok=True)

    # 整篇预缓存:先数出需要音频的句总数(read_aloud 且有文本),用于进度显示。
    total_targets = 0
    if not args.only:
        for s in iter_sentences(note.get("blocks", [])):
            if s.get("id") and s.get("read_aloud", True) and tts_text(s, read_speaker):
                total_targets += 1

    total = done = skipped_exist = skipped_noread = failed = 0
    only_out = None

    def emit_progress():
        # 机械可读进度行,给 Rust 解析后转发前端(已处理数=新生成+已存在+失败)。
        if not args.only:
            print(f"@PROGRESS {done + skipped_exist + failed} {total_targets}", flush=True)

    for s in iter_sentences(note.get("blocks", [])):
        sid = s.get("id")
        if not sid:
            continue
        if args.only and sid != args.only:
            continue  # --only 模式:只处理指定句
        total += 1
        if not s.get("read_aloud", True):
            skipped_noread += 1
            continue
        text = tts_text(s, read_speaker)
        if not text:
            skipped_noread += 1
            continue
        # 文件名 = 句序号(sid 形如 note_xxx_5 → 5.mp3);audio 写回相对 media/ 的路径(含音色层)。
        seq = sid.rsplit("_", 1)[-1]
        out = media_dir / f"{seq}.{out_ext}"
        rel_audio = f"{note_id}/{vkey}/{out.name}"
        if not args.only and out.exists() and out.stat().st_size > 0:
            # 已存在也要把正确的相对路径写回 JSON(可能从别的厂商换过来 mp3↔wav)
            s["audio"] = rel_audio
            skipped_exist += 1
            emit_progress()
            continue
        try:
            synth(text, out)
            s["audio"] = rel_audio
            done += 1
            if args.only:
                only_out = str(out.resolve())
        except Exception as e:
            failed += 1
            sys.stderr.write(f"  ✗ {sid}: {e}\n")
            if out.exists():
                out.unlink()
        emit_progress()

    # 仅「整篇」模式把 audio 路径写回 JSON(作为默认音色指针)。
    # --only 是实时/单句按需合成:只填该音色的缓存文件,不改 JSON,避免篡改默认指针。
    if not args.only:
        note_path.write_text(json.dumps(note, ensure_ascii=False, indent=2), encoding="utf-8")

    # --only 模式:stdout 最后一行打印生成的文件绝对路径(给后端读)
    if args.only and only_out:
        print(only_out, flush=True)

    print(
        f"完成:共 {total} 句 | 新生成 {done} | 已存在跳过 {skipped_exist} | "
        f"不朗读跳过 {skipped_noread} | 失败 {failed}",
        flush=True,
    )
    sys.exit(0 if failed == 0 else 1)


if __name__ == "__main__":
    main()
