# 内置 FFmpeg 产物目录

这个目录必须随源码保留：`tauri.conf.json` 把它声明为打包资源，而 Tauri 在普通
`cargo build` 时也会校验资源源目录存在。若只在发布流水线生成目录，全新检出的
源码会在构建阶段报 `resource path vendor-ffmpeg doesn't exist`。

发布流水线会用 `scripts/ffmpeg-build/build-linux.sh` 在这里生成 FFmpeg、FFprobe
及许可证文件。生成物体积较大且可重建，均由仓库根目录的 `.gitignore` 忽略。
