#!/usr/bin/env bash
# 生成极速看片冒烟测试用的测试媒体（ffmpeg）
# 目录结构模拟真实使用：多级目录 + 自然排序 + 外挂字幕 + HLS
set -e
cd "$(dirname "$0")"
OUT=/home/z/my-project/testmedia
rm -rf "$OUT"
mkdir -p "$OUT/剧集/第01季" "$OUT/剧集/第02季" "$OUT/hls"

FF="ffmpeg -hide_banner -loglevel error -y"

# 字幕源（SRT，中文 + 英文）
cat > "$OUT/sub-zh.srt" <<'EOF'
1
00:00:00,500 --> 00:00:03,000
欢迎观看极速看片

2
00:00:03,500 --> 00:00:07,000
这是第一条内嵌字幕（中文）

3
00:00:07,500 --> 00:00:11,000
多音轨与字幕切换测试

4
00:00:11,500 --> 00:00:15,000
画面旋转与缩放测试

5
00:00:15,500 --> 00:00:19,000
断点记忆测试

6
00:00:19,500 --> 00:00:23,000
再见
EOF

cat > "$OUT/sub-en.srt" <<'EOF'
1
00:00:00,500 --> 00:00:03,000
Welcome to Jisu Kanpian

2
00:00:03,500 --> 00:00:07,000
This is embedded subtitle (English)

3
00:00:07,500 --> 00:00:11,000
Multi audio track test

4
00:00:11,500 --> 00:00:15,000
Rotate and zoom test

5
00:00:15,500 --> 00:00:19,000
Position memory test

6
00:00:19,500 --> 00:00:23,000
Goodbye
EOF

# ---------- 1. 多音轨 + 内嵌 mov_text 字幕 MP4（moov 在文件尾，非 faststart）----------
$FF -f lavfi -i "testsrc2=size=640x360:rate=24:duration=24" \
    -f lavfi -i "sine=frequency=440:duration=24" \
    -f lavfi -i "sine=frequency=880:duration=24" \
    -f lavfi -i "sine=frequency=220:duration=24" \
    -i "$OUT/sub-zh.srt" -i "$OUT/sub-en.srt" \
    -map 0:v -map 1:a -map 2:a -map 3:a -map 4:s -map 5:s \
    -c:v libx264 -preset veryfast -crf 28 -pix_fmt yuv420p \
    -c:a aac -b:a 64k \
    -c:s mov_text -c:s:1 mov_text \
    -metadata:s:a:0 language=chi -metadata:s:a:0 title="国语" \
    -metadata:s:a:1 language=eng -metadata:s:a:1 title="English" \
    -metadata:s:a:2 language=jpn -metadata:s:a:2 title="日本語" \
    -metadata:s:s:0 language=chi -metadata:s:s:0 title="简体中文" \
    -metadata:s:s:1 language=eng -metadata:s:s:1 title="English" \
    "$OUT/剧集/第01季/multi.mp4"

# 外挂字幕（同名多语言）：vtt 直接挂载 / srt 转换挂载
cat > "$OUT/剧集/第01季/multi.zh.vtt" <<'EOF'
WEBVTT

00:00.500 --> 00:00.03.000
外挂字幕：简体中文

00:00.03.500 --> 00:00.07.000
这条来自同名 .zh.vtt 文件
EOF
cat > "$OUT/剧集/第01季/multi.zh.ass" <<'EOF'
[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, Bold, Outline
Style: Default,Arial,20,&H00FFFFFF,0,2

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:00.50,0:00:03.00,Default,,0,0,0,,{\i1}外挂ASS字幕{\i0}斜体测试
Dialogue: 0,0:00:03.50,0:00:07.00,Default,,0,0,0,,外挂ASS第二行
EOF
cp "$OUT/sub-en.srt" "$OUT/剧集/第01季/multi.en.srt"

# ---------- 2. 单音轨 MP4（faststart，moov 在文件头）----------
$FF -f lavfi -i "testsrc2=size=640x360:rate=24:duration=16" \
    -f lavfi -i "sine=frequency=600:duration=16" \
    -c:v libx264 -preset veryfast -crf 28 -pix_fmt yuv420p \
    -c:a aac -b:a 64k -movflags +faststart \
    "$OUT/剧集/第01季/single-fast.mp4"

# ---------- 3. 单音轨 MP4（moov 在尾）+ 中文外挂 srt ----------
$FF -f lavfi -i "smptebars=size=640x360:rate=24:duration=12" \
    -f lavfi -i "sine=frequency=500:duration=12" \
    -c:v libx264 -preset veryfast -crf 28 -pix_fmt yuv420p \
    -c:a aac -b:a 64k \
    "$OUT/剧集/第02季/single-tail.mp4"
cp "$OUT/sub-zh.srt" "$OUT/剧集/第02季/single-tail.srt"

# ---------- 4. WebM（VP9 + Opus）----------
$FF -f lavfi -i "testsrc2=size=640x360:rate=24:duration=10" \
    -f lavfi -i "sine=frequency=330:duration=10" \
    -c:v libvpx-vp9 -b:v 200k -c:a libopus -b:a 48k \
    "$OUT/剧集/第02季/clip.webm"

# ---------- 4b. 多编码多音轨 MP4（AAC + Opus + AC3）：验证切轨 SourceBuffer 重建与不支持编码禁用 ----------
$FF -f lavfi -i "testsrc2=size=640x360:rate=24:duration=14" \
    -f lavfi -i "sine=frequency=440:duration=14" \
    -f lavfi -i "sine=frequency=880:duration=14" \
    -f lavfi -i "sine=frequency=220:duration=14" \
    -map 0:v -map 1:a -map 2:a -map 3:a \
    -c:v libx264 -preset veryfast -crf 28 -pix_fmt yuv420p \
    -c:a:0 aac -b:a 64k -c:a:1 libopus -b:a 48k -c:a:2 ac3 -b:a 96k \
    -metadata:s:a:0 language=chi -metadata:s:a:0 title="国语" \
    -metadata:s:a:1 language=eng -metadata:s:a:1 title="English" \
    -metadata:s:a:2 language=jpn -metadata:s:a:2 title="AC3" \
    "$OUT/剧集/第01季/multicodec.mp4"

# ---------- 4c. 多音轨 + 双字幕轨 MKV（ASS + SRT）：验证 MKV 分析与字幕提取 ----------
# 视频 / 音频取 24s（字幕最长 23s）：避免「容器时长 > 视频流时长」
# 导致快进触底后停在无数据区间、不触发 ended 连播
cat > "$OUT/sub-ass.ass" <<'EOF'
[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, Bold, Outline
Style: Default,Arial,20,&H00FFFFFF,0,2

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:00.50,0:00:03.00,Default,,0,0,0,,{\i1}内嵌ASS字幕{\i0}第一行\N第二行
Dialogue: 0,0:00:03.50,0:00:07.00,Default,,0,0,0,,Second ASS line with {\b1}bold{\b0}
Dialogue: 0,0:00:07.50,0:00:11.00,Default,,0,0,0,,多音轨 MKV 测试
Dialogue: 0,0:00:11.50,0:00:14.00,Default,,0,0,0,,再见
EOF
$FF -f lavfi -i "testsrc2=size=640x360:rate=24:duration=24" \
    -f lavfi -i "sine=frequency=440:duration=24" \
    -f lavfi -i "sine=frequency=880:duration=24" \
    -i "$OUT/sub-ass.ass" -i "$OUT/sub-zh.srt" \
    -map 0:v -map 1:a -map 2:a -map 3:s -map 4:s \
    -c:v libx264 -preset veryfast -crf 28 -pix_fmt yuv420p \
    -c:a:0 aac -b:a 64k -c:a:1 libmp3lame -b:a 64k \
    -c:s:0 copy -c:s:1 srt \
    -metadata:s:a:0 language=chi -metadata:s:a:0 title="国语" \
    -metadata:s:a:1 language=eng -metadata:s:a:1 title="English" \
    -metadata:s:s:0 language=chi -metadata:s:s:0 title="简体中文" \
    -metadata:s:s:1 language=eng -metadata:s:s:1 title="English" \
    "$OUT/剧集/第01季/multi.mkv"
rm -f "$OUT/sub-ass.ass"


# ---------- 5. HLS 流（本地目录版 m3u8 + ts 分片）----------
$FF -f lavfi -i "testsrc2=size=640x360:rate=24:duration=18" \
    -f lavfi -i "sine=frequency=440:duration=18" \
    -c:v libx264 -preset veryfast -crf 28 -pix_fmt yuv420p \
    -c:a aac -b:a 64k \
    -f hls -hls_time 4 -hls_list_size 0 \
    -hls_segment_filename "$OUT/hls/seg%03d.ts" \
    "$OUT/hls/playlist.m3u8"

rm -f "$OUT/sub-zh.srt" "$OUT/sub-en.srt"
echo "==== 测试媒体生成完毕 ===="
find "$OUT" -type f | sort
