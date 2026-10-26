import { useState } from 'react';
import { api, type SettingsView } from '../api';
import { t } from '../lib/i18n.ts';
import { Dialog } from './Dialog';
import { toast } from './Toast';

/** 弹框里填一个 OpenAI 兼容的 AI 服务。密钥只进系统钥匙串，保存后界面上不再显示它。保存即选用它。 */
export function ApiDialog({ s, onClose }: { s: SettingsView; onClose: () => void }) {
  const [baseUrl, setBaseUrl] = useState(s.api?.base_url ?? '');
  const [model, setModel] = useState(s.api?.model ?? '');
  const [key, setKey] = useState('');
  const [err, setErr] = useState('');
  const save = () =>
    api
      .aiApiSet(baseUrl, model, key || null)
      .then(() => api.aiBackendSet('api_key'))
      .then(
        () => {
          toast(t('已保存'));
          onClose();
        },
        (e) => setErr(String(e)),
      );
  return (
    <Dialog
      title={t('自定义模型')}
      actions={
        <>
          {s.api && (
            <button className="btn gray" style={{ marginRight: 'auto', color: 'var(--red)' }} onClick={() => api.aiApiClear().then(onClose, (e) => setErr(String(e)))}>
              {t('删除')}
            </button>
          )}
          <button className="btn gray" onClick={onClose}>
            {t('取消')}
          </button>
          <button className="btn" onClick={save}>
            {t('保存')}
          </button>
        </>
      }
    >
      <p>{t('填一个 OpenAI 兼容格式的 AI 服务。密钥只保存在系统钥匙串里。')}</p>
      <label className="field">
        {t('服务地址')}
        <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="https://…/v1" spellCheck={false} autoFocus />
      </label>
      <label className="field">
        {t('模型名')}
        <input value={model} onChange={(e) => setModel(e.target.value)} spellCheck={false} />
      </label>
      <label className="field">
        API Key{s.api_key_set ? t('（已保存，留空表示不改）') : ''}
        <input type="password" value={key} onChange={(e) => setKey(e.target.value)} autoComplete="off" />
      </label>
      {err && (
        <small role="alert" style={{ color: 'var(--red)' }}>
          {err}
        </small>
      )}
    </Dialog>
  );
}
