# Pika Clean

[English](README.md) · 简体中文

开源的 macOS 清理软件：菜单栏系统监控、清理空间，以及用 AI 解释「这是什么、能不能删」。

![Pika Clean 演示：扫描后点一下，AI 解释缓存能不能删](website/demo.gif)

## 功能

- **智能扫描与清理**：缓存、日志、废纸篓、旧下载、崩溃转储、浏览器记录和 Cookie，一键找出来；攒多了每周提醒一次。
- **AI 解释**：拿不准的文件点一下，AI 告诉你它是什么、删了会怎样；也可以一句话说「帮我腾出空间，别动下载的文件」。
- **卸载软件**：连同偏好设置、缓存和支持文件一起删干净；还能查哪些应用有新版本。
- **优化速度**：释放内存、管理开机启动项、结束占资源的进程。
- **菜单栏监控**：CPU、GPU、内存、磁盘、网络、电池、温度、风扇；也可以把用量和网速直接显示在菜单栏上，任何地方按 ⌃⌥P 打开主窗口。
- **开发者深度清理**：Xcode、模拟器、Docker、包管理器，以及 Claude、Cursor 等 AI 工具的缓存。
- **空间去向**：看清整个启动盘的空间都花在哪了。

## 安装

- 从[官网](https://pika-clean.vercel.app)下载 dmg，同时支持 Intel 和 Apple 芯片的 Mac。
- 或者自己编译，见下面的「常用命令」。

应用目前还没有苹果开发者签名，第一次打开会被系统拦住：

1. 把 Pika Clean 拖进「应用程序」文件夹，打开一次。
2. 被拦住后，到「系统设置 → 隐私与安全性」，在页面底部点「仍要打开」，再输入一次登录密码。以后不会再提示。
3. 如果提示「已损坏，应移到废纸篓」，在「终端」里执行下面这行，然后重新打开：

```bash
xattr -cr "/Applications/Pika Clean.app"
```

全部功能免费。以后可能推出可选的付费服务（比如不用自己配 Key 的 AI 额度），现有功能会一直免费。

## AI 用哪家

两种来源，在「设置 → AI」里选：

- **本机 Claude Code**：装好并登录过就能直接用，用的是你自己的额度。
- **自定义模型**：任何 OpenAI 兼容格式的服务都行。没装 Claude Code 时，第一次用 AI 会直接弹出填写框。

常见服务的「服务地址」（模型名以各家文档为准）：

| 服务 | 服务地址 |
|---|---|
| DeepSeek | `https://api.deepseek.com/v1` |
| 通义千问（阿里云百炼） | `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| Kimi（月之暗面） | `https://api.moonshot.cn/v1` |
| OpenRouter | `https://openrouter.ai/api/v1` |
| OpenAI | `https://api.openai.com/v1` |
| 本机 Ollama | `http://localhost:11434/v1`（API Key 随便填一个） |

服务地址必须是 `https://`，本机地址除外。

## 隐私

- **API Key 只存在 macOS 系统钥匙串里**，不写进任何配置文件，保存后界面上也不再显示。
- 只有你主动点 AI 功能时才调用，不会在后台偷偷用你的额度。
- 发给 AI 的只有条目的路径、大小、类别、上次使用时间，或进程的名称和占用，**不发送任何文件内容**。第一次用之前会先问你同不同意，随时可以在设置里撤回。
- 删除前都要你确认。缓存、日志这类能重新生成的直接删；下载、大文件、旧项目这类个人文件只进废纸篓，清理后还能「放回原处」。

## 常用命令

都在仓库根目录运行：

```bash
make dev        # 启动开发环境：改代码自动热更新
make test       # 前端和后端的全部测试
make package    # 打包成 .app 和 .dmg
```

用自己的证书签名：在不提交的 `Makefile.local` 里写一行 `APPLE_SIGNING_IDENTITY = ...`。不写就用临时签名，只能在本机运行。

发布新版本：

```bash
make release NOTES="这次改了什么" VERSION=next    # 版本号末位加一：0.1.1 → 0.1.2
make release  VERSION=0.2.0   # 指定版本号
make release NOTES="这次改了什么"                 # 沿用 backend/tauri.conf.json 里的版本号，R2 上同名文件直接覆盖
```

- 已经装了某个版本的用户，只有版本号变大才会收到更新。同一个版本重发只适合修补刚发出去、还没人装的包；在终端里这样发会先要你确认。
- 发布完会把官网 `website/index.html` 里的版本号和下载链接改成这一版，官网要另外重新部署。

它会打包、把安装包传到 Cloudflare R2（`dl.daoly.top`，桶名默认 `pika-clean-releases`，可用 `R2_BUCKET=` 改），最后更新 `latest.json`；已装的应用下次启动会弹窗提示更新。第一次用先 `npx wrangler login`。签名私钥在 `~/.tauri/pikaclean_updater.key`，务必备份。

开发端口默认 18540。被别的项目占用时换一个：

```bash
make dev PORT=18541
```

## 目录

```
frontend/   界面（React + TypeScript）
backend/    Rust + Tauri：系统指标、扫描、删除、AI 调用
website/    产品官网（静态页面）
```

## 在真实应用里自测

应用能自己走一遍全流程（扫描、清理、卸载、优化、设置、几次真实的 AI 调用），导出每一步的画面和页面上的文字：

```bash
cd backend
cargo build --release --features selftest,tauri/custom-protocol
./target/release/pikaclean --selftest /tmp/pika-selftest
```

- 大约五分钟。窗口放在屏幕外、不抢焦点、不显示菜单栏图标，运行时看不到它，也不影响你继续用电脑。
- 它用单独的数据目录（`~/Library/Application Support/com.pikaclean.mac.selftest`），不碰真实的设置和清理记录。
- 它只会真实删除自己放进去的一个测试目录和一个假应用。开机启动项也只开关、移除它自己放进去的一个假启动代理和一个假登录项，结束时清干净。
- 前面加 `PIKA_SELFTEST_FAST=1` 可以一分钟跑完：AI 的回答用上一次留下的缓存。
- 会消耗几次 Claude Code 的调用额度。
- 截不到菜单栏图标、窗口毛玻璃和系统密码框，这些要人眼看。

## 支持作者

Pika Clean 全部功能免费。觉得好用的话，可以在[爱发电](https://afdian.com/a/black-robe-sanfeng)赞助。赞助不解锁任何功能，会用来支付苹果开发者签名费用和后续开发。

## 许可证

代码以 [GPLv3](LICENSE) 开源：可以自由使用、修改和分发，但修改后分发的版本也必须以 GPLv3 开源。

「Pika Clean」名称和应用图标不在开源许可范围内。基于本项目分发修改版时，请换用自己的名称和图标。

[`website/`](website/) 目录下的文案和图片保留所有权利，不在 GPLv3 范围内。

