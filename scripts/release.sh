#!/bin/sh
# 打包并传到 Cloudflare R2，最后更新 latest.json：已装的应用下次启动就会提示更新。
# 由 make release 调用，第一次用先运行 npx wrangler login。
#   make release                    沿用 backend/tauri.conf.json 里的版本号；同一个版本再发一次，R2 上的同名文件直接覆盖（在终端里跑会先要你确认）
#   make release VERSION=0.2.0      先把版本号改成 0.2.0 再发布
#   make release VERSION=next       末位加一（0.1.1 → 0.1.2）再发布
#   NOTES="这次改了什么"            更新说明，弹窗里会显示
# 先传安装包、最后传 latest.json，这样应用永远不会查到一个还没传上去的版本。
# 发布完会把官网 website/index.html 里的版本号改成这一版，官网要另外 make website 重新部署。
set -eu
BUCKET=${R2_BUCKET:-pika-clean-releases}
BASE=https://dl.daoly.top
DIR=release-artifacts
CONF=backend/tauri.conf.json

CUR=$(node -p "require('./$CONF').version")
case "${VERSION:-}" in
  "") V=$CUR ;;
  next) V=$(echo "$CUR" | awk -F. '{print $1"."$2"."$3+1}') ;;
  *) V=$VERSION ;;
esac
echo "$V" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { echo "版本号要写成 x.y.z，或者 next：$V"; exit 1; }
if [ "$V" != "$CUR" ]; then
  sed -i '' "s/\"version\": \"$CUR\"/\"version\": \"$V\"/" "$CONF"
  echo "版本号 $CUR → $V"
else
  echo ""
  echo "!!!!!!!! 注意：沿用当前版本号 $V 重新发布，R2 上的同名文件会被覆盖 !!!!!!!!"
  echo "!!!!!!!! 已经装了 $V 的用户收不到这次发布（只有版本号变大才算有更新）。要让他们收到，用 VERSION=next 或 VERSION=x.y.z"
  # 在终端里跑时要确认一下；不是在终端里（比如脚本调用）照旧直接发
  if [ -t 0 ]; then
    printf "确定沿用 %s 重新发布？输入 y 继续：" "$V"
    read -r ANSWER
    [ "$ANSWER" = y ] || { echo "已取消"; exit 1; }
  fi
fi

make package

# universal 包：Intel 和 Apple 芯片共用同一个文件
TGZ=pikaclean-$V-universal.app.tar.gz
DMG=pikaclean-$V-universal.dmg
# 一律不让 CDN 缓存：同一个版本重发时文件名不变，缓存住旧包会和 latest.json 里的新签名对不上，更新就装不上
put() { npx --yes wrangler@4 r2 object put "$BUCKET/$1" --file "$2" --remote --cache-control no-cache; }

put "$TGZ" "$DIR/Pika Clean.app.tar.gz"
put "$DMG" "$DIR"/*.dmg
V=$V URL=$BASE/$TGZ node -e '
const e = process.env;
const signature = require("fs").readFileSync("release-artifacts/Pika Clean.app.tar.gz.sig", "utf8");
// 两种芯片指向同一个 universal 包；老版本（只打了 aarch64）读的也是 darwin-aarch64，照样能收到更新
const mac = { signature, url: e.URL };
console.log(JSON.stringify({ version: e.V, notes: e.NOTES || "", pub_date: new Date().toISOString(), platforms: { "darwin-aarch64": mac, "darwin-x86_64": mac } }, null, 2));
' > "$DIR/latest.json"
put latest.json "$DIR/latest.json"

# 官网的下载链接和显示的版本号跟着改；官网要重新部署才生效
sed -i '' "s/^const VERSION = '[0-9.]*';/const VERSION = '$V';/" website/index.html
# 0.1.3 及以前只有 aarch64 包；从 universal 包开始，官网的下载链接跟着换后缀
sed -i '' 's/-aarch64\.dmg/-universal.dmg/' website/index.html

echo "已发布 $V"
echo "新用户下载：$BASE/$DMG"
echo "官网 website/index.html 的版本号已改成 $V，记得 make website 重新部署官网"
