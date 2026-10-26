//! 后端展示给用户的文字的翻译：报错、系统通知、密码框里的说明、菜单栏图标的右键菜单。
//! 和前端一个做法：源码里直接写简体中文，它同时是查表的键；表里没有的原样返回中文。

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lang {
    ZhHans,
    ZhHant,
    En,
    Ja,
    Ko,
}

const ALL: [Lang; 5] = [Lang::ZhHans, Lang::ZhHant, Lang::En, Lang::Ja, Lang::Ko];

impl Lang {
    /// 与前端 `Lang` 类型一致的代号，也是存进设置文件的值。
    pub fn code(self) -> &'static str {
        match self {
            Lang::ZhHans => "zh-Hans",
            Lang::ZhHant => "zh-Hant",
            Lang::En => "en",
            Lang::Ja => "ja",
            Lang::Ko => "ko",
        }
    }

    pub fn parse(code: &str) -> Option<Lang> {
        ALL.into_iter().find(|l| l.code() == code)
    }

    /// 系统语言标识（如 `zh-Hant-TW`、`en-CN`、`ja-JP`）对应哪种界面语言；不认识的返回 None。
    fn from_locale(tag: &str) -> Option<Lang> {
        let tag = tag.to_ascii_lowercase();
        let lang = tag.split(['-', '_']).next()?;
        Some(match lang {
            "zh" if ["hant", "-tw", "-hk", "-mo"].iter().any(|s| tag.contains(s)) => Lang::ZhHant,
            "zh" => Lang::ZhHans,
            "en" => Lang::En,
            "ja" => Lang::Ja,
            "ko" => Lang::Ko,
            _ => return None,
        })
    }

    /// 追加在 AI 系统提示词后面的一句，让它用界面语言回答。提示词本身一直是中文。
    pub fn ai_instruction(self) -> &'static str {
        match self {
            Lang::ZhHans => "\n\n请用简体中文回答。",
            Lang::ZhHant => "\n\n请用繁體中文（台灣用語）回答。JSON 的键名和 category 等取值保持原样，不要翻译。",
            Lang::En => "\n\n请用 English 回答。JSON 的键名和 category 等取值保持原样，不要翻译。",
            Lang::Ja => "\n\n请用日本語回答。JSON 的键名和 category 等取值保持原样，不要翻译。",
            Lang::Ko => "\n\n请用한국어回答。JSON 的键名和 category 等取值保持原样，不要翻译。",
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn current() -> Lang {
    ALL[CURRENT.load(Ordering::Relaxed) as usize]
}

pub fn set(lang: Lang) {
    CURRENT.store(ALL.iter().position(|l| *l == lang).unwrap_or(0) as u8, Ordering::Relaxed);
}

/// 用户在设置里选的语言；没选（跟随系统）时取系统首选语言里第一个支持的，都不支持就用英文。
pub fn resolve(choice: Option<&str>) -> Lang {
    choice.and_then(Lang::parse).unwrap_or_else(|| pick(&system_languages()))
}

fn pick(tags: &[String]) -> Lang {
    tags.iter().find_map(|t| Lang::from_locale(t)).unwrap_or(Lang::En)
}

/// 系统「语言与地区」里的首选语言列表，按优先顺序。读不到时为空。
fn system_languages() -> Vec<String> {
    let out = std::process::Command::new("/usr/bin/defaults").args(["read", "-g", "AppleLanguages"]).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    parse_languages(&text)
}

/// `defaults` 的输出形如 `(\n    "zh-Hans-CN",\n    "en-CN"\n)`，引号在只有字母数字时会省略。
fn parse_languages(text: &str) -> Vec<String> {
    text.lines().map(|l| l.trim().trim_end_matches(',').trim_matches('"').to_owned()).filter(|l| !l.is_empty() && l != "(" && l != ")").collect()
}

/// 翻译一句。带变量的句子用 `{0}` 占位，调用方自己 `replace`。
pub fn t(zh: &'static str) -> &'static str {
    let i = match current() {
        Lang::ZhHans => return zh,
        Lang::ZhHant => 0,
        Lang::En => 1,
        Lang::Ja => 2,
        Lang::Ko => 3,
    };
    TABLE.iter().find(|(k, _)| *k == zh).map_or(zh, |(_, tr)| tr[i])
}

/// 简体原文 → [繁體中文, English, 日本語, 한국어]。
/// 前五条会出现在系统密码框里，被拼进 AppleScript 字符串，所以不能含双引号和反斜杠（有测试看着）。
#[rustfmt::skip]
const TABLE: &[(&str, [&str; 4])] = &[
    ("Pika Clean 要释放未使用的内存。", ["Pika Clean 要釋放未使用的記憶體。", "Pika Clean wants to free unused memory.", "Pika Clean が未使用のメモリを解放しようとしています。", "Pika Clean이 사용하지 않는 메모리를 해제하려고 합니다."]),
    ("Pika Clean 要刷新 DNS 缓存。", ["Pika Clean 要清除 DNS 快取。", "Pika Clean wants to flush the DNS cache.", "Pika Clean が DNS キャッシュをクリアしようとしています。", "Pika Clean이 DNS 캐시를 비우려고 합니다."]),
    ("Pika Clean 要重建聚焦搜索的索引。", ["Pika Clean 要重建 Spotlight 的索引。", "Pika Clean wants to rebuild the Spotlight index.", "Pika Clean が Spotlight の索引を再構築しようとしています。", "Pika Clean이 Spotlight 색인을 다시 만들려고 합니다."]),
    ("Pika Clean 要回收本地快照占用的可清除空间。", ["Pika Clean 要回收本機快照佔用的可清除空間。", "Pika Clean wants to reclaim purgeable space used by local snapshots.", "Pika Clean がローカルスナップショットの占めるパージ可能な領域を回収しようとしています。", "Pika Clean이 로컬 스냅샷이 차지한 제거 가능한 공간을 회수하려고 합니다."]),
    ("Pika Clean 要清理所有用户共用的系统缓存。", ["Pika Clean 要清理所有使用者共用的系統快取。", "Pika Clean wants to clean system caches shared by all users.", "Pika Clean がすべてのユーザが共有するシステムキャッシュをクリーンアップしようとしています。", "Pika Clean이 모든 사용자가 공유하는 시스템 캐시를 정리하려고 합니다."]),
    ("Pika Clean 要设置以后释放内存和刷新 DNS 缓存时不再询问密码。", ["Pika Clean 要設定以後釋放記憶體和清除 DNS 快取時不再詢問密碼。", "Pika Clean wants to stop asking for your password when freeing memory or flushing the DNS cache.", "Pika Clean が、今後メモリ解放と DNS キャッシュのクリアでパスワードを求めないように設定しようとしています。", "Pika Clean이 앞으로 메모리 해제와 DNS 캐시 비우기에 암호를 묻지 않도록 설정하려고 합니다."]),
    ("Pika Clean 要撤销免密授权。", ["Pika Clean 要撤銷免密碼授權。", "Pika Clean wants to remove its password-free permission.", "Pika Clean がパスワード不要の許可を取り消そうとしています。", "Pika Clean이 암호 없이 실행하는 권한을 취소하려고 합니다."]),
    ("要删除的条目数量不对", ["要刪除的項目數量不對", "The number of items to delete is wrong", "削除する項目の数が正しくありません", "삭제할 항목 수가 올바르지 않습니다"]),
    ("拒绝以管理员身份删除这个路径：", ["拒絕以管理員身分刪除這個路徑：", "Refused to delete this path as administrator: ", "管理者としてこのパスを削除することを拒否しました：", "관리자 권한으로 이 경로를 삭제하는 것을 거부했습니다: "]),
    ("Pika Clean 要删除程序崩溃时留下的内存转储。", ["Pika Clean 要刪除程式當機時留下的記憶體傾印。", "Pika Clean wants to delete memory dumps left by crashed programs.", "Pika Clean がプログラムのクラッシュ時に残されたメモリダンプを削除しようとしています。", "Pika Clean이 프로그램 충돌 시 남은 메모리 덤프를 삭제하려고 합니다."]),
    ("Pika Clean 要清理系统缓存和程序崩溃时留下的内存转储。", ["Pika Clean 要清理系統快取和程式當機時留下的記憶體傾印。", "Pika Clean wants to clean system caches and memory dumps left by crashed programs.", "Pika Clean がシステムキャッシュとプログラムのクラッシュ時に残されたメモリダンプをクリーンアップしようとしています。", "Pika Clean이 시스템 캐시와 프로그램 충돌 시 남은 메모리 덤프를 정리하려고 합니다."]),
    ("还没有扫描结果", ["還沒有掃描結果", "No scan results yet", "スキャン結果はまだありません", "아직 스캔 결과가 없습니다"]),
    ("请先把 Pika Clean 拖进「应用程序」文件夹，从那里打开后再开启开机自启", ["請先把 Pika Clean 拖進「應用程式」檔案夾，從那裡打開後再開啟登入時啟動", "Move Pika Clean to the Applications folder and open it from there before turning on Open at Login", "Pika Clean を「アプリケーション」フォルダに移動し、そこから開いてからログイン時に起動をオンにしてください", "Pika Clean을 「응용 프로그램」 폴더로 옮기고 거기서 연 다음 로그인 시 열기를 켜세요"]),
    ("用量记录还不够两天", ["用量記錄還不夠兩天", "Less than two days of usage history so far", "使用量の記録がまだ 2 日分ありません", "사용량 기록이 아직 이틀이 안 됩니다"]),
    ("清理已完成，但写入清理记录失败：", ["清理已完成，但寫入清理記錄失敗：", "Cleaning finished, but the history could not be saved: ", "クリーンアップは完了しましたが、履歴を保存できませんでした：", "정리는 끝났지만 기록을 저장하지 못했습니다: "]),
    ("服务地址必须以 https:// 开头（本机地址除外）", ["服務位址必須以 https:// 開頭（本機位址除外）", "The base URL must start with https:// (except local addresses)", "サービス URL は https:// で始まる必要があります（ローカルアドレスを除く）", "서비스 주소는 https://로 시작해야 합니다(로컬 주소 제외)"]),
    ("请填写模型名", ["請填寫模型名稱", "Enter a model name", "モデル名を入力してください", "모델 이름을 입력하세요"]),
    ("请填写 API Key", ["請填寫 API Key", "Enter an API key", "API キーを入力してください", "API 키를 입력하세요"]),
    ("模型名只能包含字母、数字和 . _ - : [ ]", ["模型名稱只能包含字母、數字和 . _ - : [ ]", "A model name may contain only letters, digits and . _ - : [ ]", "モデル名に使えるのは英数字と . _ - : [ ] だけです", "모델 이름에는 영문자, 숫자와 . _ - : [ ] 만 쓸 수 있습니다"]),
    ("条目不在扫描结果里", ["項目不在掃描結果裡", "The item is not in the scan results", "項目がスキャン結果にありません", "항목이 스캔 결과에 없습니다"]),
    ("还没有系统指标", ["還沒有系統指標", "System metrics are not available yet", "システム指標はまだありません", "아직 시스템 지표가 없습니다"]),
    ("进程已不在列表里", ["程序已不在列表裡", "The process is no longer in the list", "プロセスはもう一覧にありません", "프로세스가 더 이상 목록에 없습니다"]),
    ("应用不在列表里", ["應用程式不在列表裡", "The app is not in the list", "アプリが一覧にありません", "앱이 목록에 없습니다"]),
    ("没有找到 Docker", ["沒有找到 Docker", "Docker was not found", "Docker が見つかりません", "Docker를 찾을 수 없습니다"]),
    ("这台模拟器已经不在了", ["這台模擬器已經不在了", "This simulator no longer exists", "このシミュレータはもうありません", "이 시뮬레이터는 더 이상 없습니다"]),
    ("这个模拟器系统版本已经不在了", ["這個模擬器系統版本已經不在了", "This simulator runtime no longer exists", "このシミュレータのランタイムはもうありません", "이 시뮬레이터 런타임은 더 이상 없습니다"]),
    ("还有模拟器在用这个系统版本", ["還有模擬器在用這個系統版本", "A simulator still uses this runtime", "このランタイムを使っているシミュレータがあります", "이 런타임을 사용하는 시뮬레이터가 있습니다"]),
    ("超时了", ["逾時了", "Timed out", "タイムアウトしました", "시간이 초과되었습니다"]),
    ("不在空间统计结果里", ["不在空間統計結果裡", "Not in the space results", "容量の集計結果にありません", "공간 집계 결과에 없습니다"]),
    ("这一项不能在空间去向里删除", ["這一項不能在空間去向裡刪除", "This item can't be deleted here", "この項目はここでは削除できません", "이 항목은 여기서 삭제할 수 없습니다"]),
    ("位置已经变了，请重新统计", ["位置已經變了，請重新統計", "The location has changed. Please rescan.", "場所が変わりました。もう一度集計してください", "위치가 바뀌었습니다. 다시 집계하세요"]),
    ("不在应用列表里", ["不在應用程式列表裡", "not in the app list", "アプリ一覧にありません", "앱 목록에 없음"]),
    ("没有旧下载文件", ["沒有舊下載檔案", "There are no old downloads", "古いダウンロードはありません", "오래된 다운로드가 없습니다"]),
    ("键盘已经锁着了", ["鍵盤已經鎖著了", "The keyboard is already locked", "キーボードはすでにロックされています", "키보드가 이미 잠겨 있습니다"]),
    ("没能锁住键盘", ["沒能鎖住鍵盤", "Could not lock the keyboard", "キーボードをロックできませんでした", "키보드를 잠그지 못했습니다"]),
    ("写入钥匙串失败：", ["寫入鑰匙圈失敗：", "Could not write to the Keychain: ", "キーチェーンに書き込めませんでした：", "키체인에 쓰지 못했습니다: "]),
    ("不能删除扫描位置本身", ["不能刪除掃描位置本身", "The scanned folder itself cannot be deleted", "スキャン対象のフォルダ自体は削除できません", "스캔 위치 자체는 삭제할 수 없습니다"]),
    ("条目已不存在：", ["項目已不存在：", "The item no longer exists: ", "項目はもう存在しません：", "항목이 더 이상 없습니다: "]),
    ("扫描位置已不存在：", ["掃描位置已不存在：", "The scanned folder no longer exists: ", "スキャン対象のフォルダはもう存在しません：", "스캔 위치가 더 이상 없습니다: "]),
    ("路径不在允许删除的范围内", ["路徑不在允許刪除的範圍內", "The path is outside what may be deleted", "削除が許可された範囲外のパスです", "삭제가 허용된 범위 밖의 경로입니다"]),
    ("路径不合法", ["路徑不合法", "Invalid path", "パスが正しくありません", "경로가 올바르지 않습니다"]),
    ("已取消输入密码", ["已取消輸入密碼", "Password entry was cancelled", "パスワード入力がキャンセルされました", "암호 입력을 취소했습니다"]),
    ("不在扫描结果里", ["不在掃描結果裡", "not in the scan results", "スキャン結果にありません", "스캔 결과에 없음"]),
    ("没能删除", ["沒能刪除", "could not be deleted", "削除できませんでした", "삭제하지 못함"]),
    ("移到废纸篓失败", ["移到垃圾桶失敗", "Could not move to the Trash", "ゴミ箱に移動できませんでした", "휴지통으로 옮기지 못했습니다"]),
    ("废纸篓里找不到它：", ["垃圾桶裡找不到它：", "Not found in the Trash: ", "ゴミ箱に見つかりません：", "휴지통에서 찾을 수 없습니다: "]),
    ("它已经不在废纸篓里了", ["它已經不在垃圾桶裡了", "It is no longer in the Trash", "もうゴミ箱にありません", "더 이상 휴지통에 없습니다"]),
    ("原来的位置已经有同名的东西了", ["原來的位置已經有同名的東西了", "Something with the same name is already in its original location", "元の場所に同じ名前のものがすでにあります", "원래 위치에 같은 이름의 항목이 이미 있습니다"]),
    ("原来所在的文件夹已经不在了", ["原來所在的資料夾已經不在了", "Its original folder no longer exists", "元のフォルダがもうありません", "원래 폴더가 더 이상 없습니다"]),
    ("扫描之后这里换成了别的文件，请重新扫描", ["掃描之後這裡換成了別的檔案，請重新掃描", "A different file is here now than when scanned. Please scan again", "スキャン後に別のファイルに置き換わりました。もう一度スキャンしてください", "스캔 후 다른 파일로 바뀌었습니다. 다시 스캔하세요"]),
    ("已停止", ["已停止", "Stopped", "停止しました", "중지됨"]),
    ("正在扫描，请等扫描完成后再清理", ["正在掃描，請等掃描完成後再清理", "A scan is in progress. Please clean after it finishes", "スキャン中です。完了してからクリーンアップしてください", "스캔 중입니다. 끝난 후 정리하세요"]),
    ("上一次清理还没结束", ["上一次清理還沒結束", "The previous cleanup has not finished yet", "前回のクリーンアップがまだ終わっていません", "이전 정리가 아직 끝나지 않았습니다"]),
    ("不在最近一次清理的记录里", ["不在最近一次清理的紀錄裡", "Not in the record of the latest cleanup", "直近のクリーンアップの記録にありません", "최근 정리 기록에 없습니다"]),
    ("应用没有退出（可能有没保存的内容），请手动退出后再卸载", ["應用程式沒有結束（可能有未儲存的內容），請手動結束後再解除安裝", "The app did not quit (it may have unsaved changes); quit it yourself, then uninstall", "アプリが終了しませんでした（未保存の内容がある可能性があります）。手動で終了してからアンインストールしてください", "앱이 종료되지 않았습니다(저장하지 않은 내용이 있을 수 있음). 직접 종료한 뒤 제거하세요"]),
    ("应用已不存在", ["應用程式已不存在", "The app no longer exists", "アプリはもう存在しません", "앱이 더 이상 없습니다"]),
    ("路径不在允许卸载的范围内", ["路徑不在允許解除安裝的範圍內", "The path is outside what may be uninstalled", "アンインストールが許可された範囲外のパスです", "제거가 허용된 범위 밖의 경로입니다"]),
    ("没有这个可推出的磁盘", ["沒有這個可退出的磁碟", "There is no such disk to eject", "取り出せるディスクが見つかりません", "추출할 수 있는 해당 디스크가 없습니다"]),
    ("不能退出这个进程", ["不能結束這個程序", "This process cannot be quit", "このプロセスは終了できません", "이 프로세스는 종료할 수 없습니다"]),
    ("没有权限退出这个进程", ["沒有權限結束這個程序", "No permission to quit this process", "このプロセスを終了する権限がありません", "이 프로세스를 종료할 권한이 없습니다"]),
    ("进程已经不在了", ["程序已經不在了", "The process is already gone", "プロセスはすでに終了しています", "프로세스가 이미 없습니다"]),
    ("退出失败", ["結束失敗", "Could not quit", "終了できませんでした", "종료하지 못했습니다"]),
    ("系统状态", ["系統狀態", "System Status", "システム状態", "시스템 상태"]),
    ("打开主窗口", ["打開主視窗", "Open Main Window", "メインウインドウを開く", "메인 윈도우 열기"]),
    ("退出 Pika Clean", ["結束 Pika Clean", "Quit Pika Clean", "Pika Clean を終了", "Pika Clean 종료"]),
    ("不支持这种语言", ["不支援這種語言", "This language is not supported", "この言語には対応していません", "지원하지 않는 언어입니다"]),
    ("磁盘空间快用完了", ["磁碟空間快用完了", "Disk space is running low", "ディスクの空き容量が残りわずかです", "디스크 공간이 거의 없습니다"]),
    ("只剩 {0} GB 可用。打开 Pika Clean 清理一下？", ["只剩 {0} GB 可用。打開 Pika Clean 清理一下？", "Only {0} GB left. Open Pika Clean to free some space?", "残り {0} GB です。Pika Clean を開いてクリーンアップしますか？", "{0} GB만 남았습니다. Pika Clean을 열어 정리할까요?"]),
    ("这一项已经不在开机启动列表里", ["這一項已經不在開機啟動列表裡", "This item is no longer in the login items list", "この項目はログイン項目の一覧にもうありません", "이 항목은 더 이상 로그인 항목 목록에 없습니다"]),
    ("全局启动代理不能移除，只能停用", ["全域啟動代理不能移除，只能停用", "System-wide launch agents can't be removed, only disabled", "システム全体の起動エージェントは削除できず、無効化のみ可能です", "시스템 전체 실행 에이전트는 제거할 수 없고 끄기만 할 수 있습니다"]),
    ("选中的不是应用", ["選中的不是應用程式", "The selected item is not an app", "選択したものはアプリではありません", "선택한 항목은 앱이 아닙니다"]),
    ("未命名", ["未命名", "Untitled", "名称なし", "이름 없음"]),
    ("服务返回 {0}：{1}", ["服務回傳 {0}：{1}", "The service returned {0}: {1}", "サービスが {0} を返しました：{1}", "서비스가 {0}을(를) 반환했습니다: {1}"]),
    ("AI 回答超过 5 分钟，已停止", ["AI 回答超過 5 分鐘，已停止", "The AI took more than 5 minutes to answer and was stopped", "AI の回答が 5 分を超えたため停止しました", "AI 답변이 5분을 넘어 중지했습니다"]),
    ("读不到登录时打开的应用：需要允许本应用控制「系统事件」", ["讀不到登入時打開的應用程式：需要允許本應用程式控制「系統事件」", "Can't read the apps that open at login: allow this app to control System Events", "ログイン時に開くアプリを読み取れません：このアプリに「システムイベント」の制御を許可してください", "로그인 시 열리는 앱을 읽을 수 없습니다: 이 앱이 '시스템 이벤트'를 제어하도록 허용하세요"]),
    ("系统级后台服务只能查看，开关和移除需要管理员权限", ["系統級背景服務只能查看，開關和移除需要管理員權限", "System-wide background services are view-only; turning them off or removing them needs admin rights", "システム全体のバックグラウンドサービスは表示のみです。オン／オフや削除には管理者権限が必要です", "시스템 전체 백그라운드 서비스는 보기만 가능하며, 켜고 끄거나 제거하려면 관리자 권한이 필요합니다"]),
];
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_first_supported_system_language() {
        let tags = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_languages("(\n    \"zh-Hans-CN\",\n    \"en-CN\",\n    ja\n)\n"), tags(&["zh-Hans-CN", "en-CN", "ja"]));
        assert_eq!(pick(&tags(&["zh-Hans-CN", "en-CN"])), Lang::ZhHans);
        assert_eq!(pick(&tags(&["zh-Hant-TW"])), Lang::ZhHant);
        assert_eq!(pick(&tags(&["zh-HK"])), Lang::ZhHant);
        assert_eq!(pick(&tags(&["fr-FR", "ko-KR", "en-US"])), Lang::Ko);
        // 都不支持、或者读不到系统语言时用英文
        assert_eq!(pick(&tags(&["fr-FR"])), Lang::En);
        assert_eq!(pick(&[]), Lang::En);
        assert_eq!(resolve(Some("ja")), Lang::Ja);
        for l in ALL {
            assert_eq!(Lang::parse(l.code()), Some(l));
        }
    }

    /// 源码里每一句 `t("…")` 都要在表里；译文的占位符和原文一致；会进密码框的句子不能破坏 AppleScript 字符串。
    #[test]
    fn table_covers_every_string_in_the_source() {
        let mut used = Vec::new();
        for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|n| n == "i18n.rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            let mut rest = src.as_str();
            while let Some(at) = rest.find("t(\"") {
                let boundary = rest[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric() && c != '_');
                rest = &rest[at + 3..];
                if let (true, Some(end)) = (boundary, rest.find('"')) {
                    used.push(rest[..end].to_owned());
                }
            }
        }
        assert!(used.len() > 30, "没扫到源码里的文案，检查一下扫描逻辑");
        let placeholders = |s: &str| (0..4).map(|i| s.matches(&format!("{{{i}}}")).count()).collect::<Vec<_>>();
        for (zh, tr) in TABLE {
            for x in tr {
                assert_eq!(placeholders(zh), placeholders(x), "占位符不一致：{zh}");
                assert!(!x.contains('"') && !x.contains('\\'), "译文里不能有双引号或反斜杠：{x}");
            }
        }
        let missing: Vec<_> = used.iter().filter(|u| u.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) && !TABLE.iter().any(|(k, _)| k == u)).collect();
        assert!(missing.is_empty(), "这些句子还没有译文：{missing:?}");
    }
}
