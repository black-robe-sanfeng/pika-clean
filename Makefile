.PHONY: dev package release test website

# Tauri 命令行装在 frontend/ 里，但要从仓库根目录运行，它才能找到 backend/tauri.conf.json
TAURI := frontend/node_modules/.bin/tauri

# 前端开发端口，默认 18540（见 docs/rules/project.md）。被占用时：make dev PORT=18541
PORT ?= 18540

# 启动开发环境：Vite 前端 + Tauri 窗口，改代码自动热更新
# 程序坞图标换成外面带圆圈的 icons/dev/，和装好的正式版区分开（菜单栏图标的圆圈见 backend/src/mascot.rs）
dev: frontend/node_modules
	PIKA_DEV_PORT=$(PORT) $(TAURI) dev --config '{"build":{"devUrl":"http://127.0.0.1:$(PORT)"},"bundle":{"icon":["icons/dev/icon.icns","icons/dev/icon.png"]}}'

# 打包签名用的证书。用固定证书签名，系统才认得出是同一个应用，重新打包后已给的权限（完全磁盘访问、文稿等）不会丢。
# 证书名含个人邮箱，不进仓库：写在不提交的 Makefile.local 里，例如
#   APPLE_SIGNING_IDENTITY = Apple Development: you@example.com (TEAMID)
# 也可以临时指定：make package APPLE_SIGNING_IDENTITY="..."。都不写就用「-」做本机临时签名。
-include Makefile.local
APPLE_SIGNING_IDENTITY ?= -

# 自动更新包的签名私钥（没有密码）。丢了它，已装的应用就再也收不到更新，记得备份。公钥在 backend/tauri.conf.json
TAURI_SIGNING_PRIVATE_KEY ?= $(HOME)/.tauri/pikaclean_updater.key

# 打包产物统一放在这里，每次打包前清空，只留这一次的
ARTIFACTS := release-artifacts

# 打包 mac 安装包（universal：一个包同时含 Intel 和 Apple 芯片两份代码），把 .dmg 和自动更新用的 .app.tar.gz（带签名）复制到 release-artifacts/
# 需要两个 Rust 目标，缺了先装上；产物在 backend/target/universal-apple-darwin/ 下
BUNDLE := backend/target/universal-apple-darwin/release/bundle
package: frontend/node_modules
	rustup target add aarch64-apple-darwin x86_64-apple-darwin
	rm -rf $(ARTIFACTS) && mkdir -p $(ARTIFACTS)
	TAURI_SIGNING_PRIVATE_KEY="$(TAURI_SIGNING_PRIVATE_KEY)" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" APPLE_SIGNING_IDENTITY="$(APPLE_SIGNING_IDENTITY)" $(TAURI) build --target universal-apple-darwin
	cp $(BUNDLE)/dmg/*.dmg $(BUNDLE)/macos/*.app.tar.gz* $(ARTIFACTS)/
	@echo "安装包："
	@ls $(ARTIFACTS)/*.dmg

# 发布新版本：打包后传到 Cloudflare R2（dl.daoly.top），已装的应用下次启动会弹窗提示更新
# make release NOTES="这次改了什么" [VERSION=0.2.0 | VERSION=next]，不写 VERSION 就沿用当前版本号并覆盖。详见 scripts/release.sh
release: frontend/node_modules
	sh scripts/release.sh

# 部署官网 website/ 到 Vercel 正式环境（pika-clean.vercel.app），用 npx 调 Vercel 命令行，免全局安装；首次要先 npx vercel login
website:
	cd website && npx --yes vercel --prod

# 前端和后端的全部测试
test: frontend/node_modules
	npm test --prefix frontend
	cargo test --manifest-path backend/Cargo.toml

frontend/node_modules: frontend/package.json
	npm install --prefix frontend
	@touch $@
