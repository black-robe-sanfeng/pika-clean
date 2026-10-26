import { useState } from 'react';
import { api, type UpdateInfo } from '../api';
import { Dialog } from './Dialog';
import { toast } from './Toast';
import { t } from '../lib/i18n.ts';

/** 有新版本时问用户要不要更新。点「立即更新」后下载、安装并自动重启。 */
export function UpdateDialog({ info, onClose }: { info: UpdateInfo; onClose: () => void }) {
  const [busy, setBusy] = useState(false);
  // 下载进度；服务器没给总大小时一直为空，只显示「正在下载」
  const [percent, setPercent] = useState<number | null>(null);
  const install = () => {
    setBusy(true);
    api.updateInstall(setPercent).then(
      // 装上了会直接重启、不会走到这里；走到这里说明弹框之后这个版本被撤回了
      () => {
        toast(t('这个版本已经撤回，暂时没有可以安装的新版本'));
        onClose();
      },
      (e) => {
        setBusy(false);
        setPercent(null);
        toast(t('更新失败：{0}', String(e)));
      },
    );
  };
  return (
    <Dialog
      title={t('发现新版本 {0}', info.version)}
      actions={
        <>
          <button className="btn gray" disabled={busy} onClick={onClose}>
            {t('稍后')}
          </button>
          <button className="btn" disabled={busy} onClick={install}>
            {!busy ? t('立即更新') : percent == null ? t('正在下载更新…') : t('正在下载更新… {0}%', percent)}
          </button>
        </>
      }
    >
      {info.notes && <p style={{ whiteSpace: 'pre-wrap' }}>{info.notes}</p>}
      <p>{t('更新后应用会自动重启。')}</p>
    </Dialog>
  );
}
