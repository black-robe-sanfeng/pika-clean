import { type CSSProperties, type ReactNode, useEffect, useRef, useState } from 'react';
import { type AdminError, type AiBackend, api, type ClaudeModel, type DockMode, type Effort, type Passwordless, type SettingsView, type UpdateInfo } from '../api';
import { ApiDialog } from '../components/ApiDialog';
import { Icon } from '../components/Icon';
import { toast } from '../components/Toast';
import { UpdateDialog } from '../components/UpdateDialog';
import { type Lang, LANGS, t } from '../lib/i18n.ts';
import { currentTheme, THEMES, setTheme } from '../lib/theme';

/** 模型名和版本号都由 Claude Code 实时给出，这里只按系列配个圆点颜色；认不出的系列用灰色。 */
const COLORS: [string, string][] = [
  ['opus', 'var(--purple)'],
  ['fable', 'var(--blue)'],
  ['sonnet', 'var(--orange)'],
  ['haiku', 'var(--green)'],
];
const colorOf = (m: ClaudeModel) => (m.value === null ? 'var(--gray)' : (COLORS.find(([k]) => m.name.toLowerCase().includes(k))?.[1] ?? 'var(--gray)'));
const labelOf = (m: ClaudeModel) => (m.value !== null ? m.name : m.name ? `${t('默认')} · ${m.name}` : t('默认'));
// ponytail: 清单要启动一次 Claude Code 才拿得到，页面开着期间只问一次；升级 Claude Code 后重开窗口才会刷新
let modelsOnce: Promise<ClaudeModel[]> | undefined;

const EFFORT: [Effort, string, string][] = [
  ['low', t('低'), t('最快，适合简单的解释')],
  ['medium', t('中'), t('速度和质量比较均衡')],
  ['high', t('高'), t('更深入的推理，适合复杂问题')],
  ['xhigh', t('超高'), t('想得更久，回答也更慢')],
  ['max', t('最高'), t('最深入，也最慢、最费额度')],
];

/** 点一下按钮，在它旁边弹出一小块浮层；点别处、滚动页面或按 Esc 收起。 */
function Pop({ label, children }: { label: ReactNode; children: (close: () => void) => ReactNode }) {
  const [at, setAt] = useState<CSSProperties | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const close = () => setAt(null);
  useEffect(() => {
    if (!at) return;
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && close();
    window.addEventListener('resize', close);
    window.addEventListener('scroll', close, true);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('resize', close);
      window.removeEventListener('scroll', close, true);
      window.removeEventListener('keydown', onKey);
    };
  }, [at]);
  const open = () => {
    const r = btn.current!.getBoundingClientRect();
    const right = window.innerWidth - r.right;
    // 用固定定位，免得被所在的圆角卡片裁掉；下方放不下就放到上方，再放不下就在浮层里滚动
    const below = window.innerHeight - r.bottom;
    setAt(below < 280 ? { bottom: window.innerHeight - r.top + 6, right, maxHeight: r.top - 18 } : { top: r.bottom + 6, right, maxHeight: below - 18 });
  };
  return (
    <>
      <button ref={btn} className="pick" aria-expanded={!!at} onClick={at ? close : open}>
        {label}
        <Icon name="chev" className="chev" />
      </button>
      {at && (
        <>
          <div className="dd-shade" onClick={close} />
          <div className="dd" style={at}>
            {children(close)}
          </div>
        </>
      )}
    </>
  );
}

/** 调用 Claude Code 时用哪个模型、推理多用力。都不设就跟随 Claude Code 自己的配置。 */
function ClaudeForm({ s, onSaved }: { s: SettingsView; onSaved: () => void }) {
  const [model, setModel] = useState(s.claude.model);
  const [effort, setEffort] = useState(s.claude.effort);
  const save = (m: string | null, e: Effort | null) => {
    setModel(m);
    setEffort(e);
    api.aiClaudeSet(m, e).then(onSaved, (err) => toast(String(err)));
  };
  const [list, setList] = useState<ClaudeModel[]>([]);
  useEffect(() => {
    (modelsOnce ??= api.aiClaudeModels().catch(() => [])).then(setList);
  }, []);
  // 清单还没拿到或拿不到时也留着「默认」；设置文件里存着清单以外的模型名时，也把它列出来
  const models = list.some((m) => m.value === null) ? [...list] : [{ value: null, name: '' }, ...list];
  if (!models.some((m) => m.value === model)) models.push({ value: model, name: model! });
  const current = models.find((m) => m.value === model)!;
  const level = EFFORT.findIndex(([id]) => id === effort);
  // 没设过强度时，滑块停在「高」的位置但不着色
  const shown = level < 0 ? 2 : level;
  return (
    <div className="row">
      <span className="grow">
        <span>{t('Claude Code 的模型和推理强度')}</span>
        <small>{t('不设就用 Claude Code 自己的默认值')}</small>
      </span>
      <Pop
        label={
          <>
            <i className="dot" style={{ '--c': colorOf(current) } as CSSProperties} />
            {labelOf(current)}
          </>
        }
      >
        {(close) =>
          models.map((m) => (
            <button
              key={m.value ?? 'default'}
              className={`dd-item ${m.value === model ? 'on' : ''}`}
              onClick={() => {
                save(m.value, effort);
                close();
              }}
            >
              <i className="dot" style={{ '--c': colorOf(m) } as CSSProperties} />
              {labelOf(m)}
            </button>
          ))
        }
      </Pop>
      <Pop label={level < 0 ? t('默认') : EFFORT[level][1]}>
        {() => (
          <div className="effort">
            <div className="eh">
              {t('推理强度')}{' '}<b>{level < 0 ? t('默认') : EFFORT[level][1]}</b>
              {level >= 0 && (
                <button className="link" onClick={() => save(model, null)}>
                  {t('恢复默认')}
                </button>
              )}
            </div>
            <div className="ends">
              <span>{t('更快')}</span>
              <span>{t('更聪明')}</span>
            </div>
            <div className={`slide ${level < 0 ? 'unset' : ''}`} style={{ '--p': shown / 4 } as CSSProperties}>
              <input type="range" min={0} max={4} step={1} value={shown} aria-label={t('推理强度')} onChange={(e) => save(model, EFFORT[Number(e.target.value)][0])} />
              {EFFORT.map(([id], k) => (
                <i key={id} style={{ visibility: k === shown ? 'hidden' : 'visible' }} />
              ))}
            </div>
            <small>{level < 0 ? t('跟随 Claude Code 自己的设置') : EFFORT[level][2]}</small>
          </div>
        )}
      </Pop>
    </div>
  );
}

/** 清理页里点了「不再提示」的条目：扫描照样列出，但不再默认勾选。在这里移出名单就恢复。 */
function Ignored({ active }: { active: boolean }) {
  const [list, setList] = useState<string[]>([]);
  useEffect(() => {
    if (active) api.ignoredList().then(setList);
  }, [active]);
  return (
    <>
      <div className="gh">{t('不再提示的条目')}</div>
      <div className="group plain">
        {list.length === 0 && (
          <div className="row">
            <span className="grow">
              <small style={{ whiteSpace: 'normal' }}>{t('还没有。在清理页的明细里右键一个条目，选「不再提示」，以后扫描就不会默认勾选它。')}</small>
            </span>
          </div>
        )}
        {list.map((p) => (
          <div className="row" key={p} title={p}>
            <span className="grow">
              <span>{p.slice(p.lastIndexOf('/') + 1)}</span>
              <small>{p.slice(0, p.lastIndexOf('/'))}</small>
            </span>
            <button className="btn gray sm" onClick={() => api.ignoreRemove(p).then(setList, (e) => toast(t('没能更改：{0}', String(e))))}>
              {t('移除')}
            </button>
          </div>
        ))}
      </div>
    </>
  );
}

const DOCK: [DockMode, string][] = [
  ['hide', t('隐藏')],
  ['show', t('显示')],
];

export function SettingsPage({ active }: { active: boolean }) {
  const [s, setS] = useState<SettingsView | null>(null);
  const [free, setFree] = useState<Passwordless | null>(null);
  const reload = () => {
    api.adminPasswordless().then(setFree);
    return api.settingsGet().then(setS);
  };
  // 每次进入都重新读：完全磁盘访问是在系统设置里改的，回来时状态可能变了
  useEffect(() => {
    if (active) reload();
  }, [active]);
  const [editApi, setEditApi] = useState(false);
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const checkUpdate = () => api.updateCheck().then((u) => (u ? setUpdate(u) : toast(t('已经是最新版本'))), (e) => toast(t('没能检查更新：{0}', String(e))));
  const [theme, setSelectedTheme] = useState(currentTheme);
  const themeNames = [t('原皮肤'), t('青绿山水'), t('水墨竹影'), t('朱砂梅韵'), t('玄金祥云'), t('敦煌莲华'), t('青花瓷')];
  // 实际在用的 AI：用户选的那个可用就用它，否则用第一个可用的（与后端的挑选规则一致）
  const backend: AiBackend | null = s ? (s.ai_backends.find((x) => x === s.ai_backend) ?? s.ai_backends[0] ?? null) : null;

  return (
    <div className="scroll">
      <h1>{t('设置')}</h1>
      {update && <UpdateDialog info={update} onClose={() => setUpdate(null)} />}

      <div className="gh">{t('通用')}</div>
      <div className="group plain">
        <div className="row">
          <span className="grow">
            <span>{t('当前版本')}</span>
            <small>{t('启动时和之后每 6 小时自动检查新版本')}</small>
          </span>
          <span>{s?.version}</span>
          <button className="btn gray sm" disabled={!s} onClick={checkUpdate}>
            {t('检查更新')}
          </button>
        </div>
        <div className="row">
          <span className="grow">
            <span>{t('支持作者')}</span>
            <small>{t('全部功能免费。觉得好用可以在爱发电赞助，赞助不解锁任何功能')}</small>
          </span>
          <button className="btn gray sm" onClick={() => api.openSponsorPage().catch((e) => toast(String(e)))}>
            {t('去赞助')}
          </button>
        </div>
        <div className="row">
          <span className="grow">
            <span>{t('菜单栏显示')}</span>
            <small>{t('在菜单栏图标旁边用文字显示用量')}</small>
          </span>
          {(['cpu', 'mem', 'disk'] as const).map((k) => (
            <label key={k} className="chk-inline">
              <input
                type="checkbox"
                checked={s?.menubar[k] ?? false}
                disabled={!s}
                onChange={(e) => s && api.menubarSet({ ...s.menubar, [k]: e.target.checked }).then(reload)}
              />
              {k === 'cpu' ? 'CPU' : k === 'mem' ? t('内存') : t('磁盘')}
            </label>
          ))}
        </div>
        <label className="row">
          <span className="grow">
            <span>{t('磁盘快满时提醒')}</span>
            <small>{t('可用空间不足一成时发一条系统通知，每天最多一次')}</small>
          </span>
          <input className="sw" type="checkbox" checked={s?.low_disk_alert ?? true} disabled={!s} onChange={(e) => api.lowDiskAlertSet(e.target.checked).then(reload)} />
        </label>
        <label className="row">
          <span className="grow">
            <span>{t('开机时自动启动')}</span>
            <small>{t('只在菜单栏显示，不打开主窗口')}</small>
          </span>
          <input
            className="sw"
            type="checkbox"
            checked={s?.autostart ?? false}
            disabled={!s}
            onChange={(e) => api.autostartSet(e.target.checked).then(reload, (err) => toast(t('没能更改：{0}', err)))}
          />
        </label>
        <div className="row">
          <span className="grow">
            <span>{t('语言')}</span>
            <small>{t('切换后界面会重新加载')}</small>
          </span>
          <Pop label={s?.language ? LANGS.find(([id]) => id === s.language)?.[1] : t('跟随系统')}>
            {(close) =>
              ([[null, t('跟随系统')], ...LANGS] as [Lang | null, string][]).map(([id, label]) => (
                <button
                  key={id ?? 'auto'}
                  className={`dd-item ${(s?.language ?? null) === id ? 'on' : ''}`}
                  onClick={() => {
                    close();
                    api.languageSet(id).catch((err) => toast(t('没能更改：{0}', String(err))));
                  }}
                >
                  {label}
                </button>
              ))
            }
          </Pop>
        </div>
        <div className="row">
          <span className="grow">
            <span>{t('显示程序坞图标')}</span>
            <small>{t('隐藏后从菜单栏图标打开主窗口')}</small>
          </span>
          <div className="seg">
            {DOCK.map(([mode, label]) => (
              <button
                key={mode}
                className={s?.dock === mode ? 'on' : ''}
                disabled={!s}
                onClick={() => api.dockModeSet(mode).then(reload, (err) => toast(t('没能更改：{0}', err)))}
              >
                {label}
              </button>
            ))}
          </div>
        </div>
      </div>

      <div className="gh">{t('主题')}</div>
      <div className="group theme-picker">
        <p>{t('选择主题，统一改变背景、色调和装饰。原皮肤随时可以切回。')}</p>
        <div className="theme-grid" role="group" aria-label={t('主题')}>
          {THEMES.map((item, index) => (
            <button
              key={item.id}
              className={`theme-option ${theme === item.id ? 'on' : ''}`}
              aria-pressed={theme === item.id}
              onClick={() => {
                try {
                  setTheme(item.id);
                  setSelectedTheme(item.id);
                } catch (err) {
                  toast(t('没能更改：{0}', String(err)));
                }
              }}
            >
              <span className={`theme-preview theme-preview-${item.id}`} style={{ '--preview-color': item.color, '--preview-paper': item.paper } as CSSProperties} aria-hidden="true">
                <span className="preview-side" />
                <span className="preview-panel"><i /><i /><i /></span>
              </span>
              <span className="theme-option-label">{themeNames[index]}<span aria-hidden="true">{theme === item.id ? '✓' : ''}</span></span>
            </button>
          ))}
        </div>
      </div>

      <div className="gh">{t('权限')}</div>
      <div className="group plain">
        <div className="row">
          <span className="grow">
            <span>{t('完全磁盘访问')}</span>
            <small>{s?.full_disk_access ? t('已授权') : t('未授权。没有它就读不了废纸篓，其余功能不受影响')}</small>
          </span>
          {s && !s.full_disk_access && (
            <button className="btn gray sm" onClick={() => api.openFullDiskAccessSettings().catch((e) => toast(String(e)))}>
              {t('去授权')}
            </button>
          )}
        </div>
        <label className="row">
          <span className="grow">
            <span>{t('释放内存和刷新 DNS 时不询问密码')}</span>
            <small>{free?.on ? t('已开启。关闭时要输一次管理员密码') : t('开启时输一次管理员密码，以后这两项不再询问。其余管理员操作仍然每次都问')}</small>
          </span>
          <input
            className="sw"
            type="checkbox"
            checked={free?.on ?? false}
            disabled={!free}
            onChange={(e) => api.adminPasswordlessSet(e.target.checked).then(reload, (err: AdminError) => err.kind === 'cancelled' || toast(t('没能更改：{0}', err.message ?? '')))}
          />
        </label>
      </div>

      <div className="gh">AI</div>
      <div className="group plain">
        <div className="row">
          <span className="grow">
            <span>{t('AI 来源')}</span>
            <small>{!s || backend ? t('用的是你自己的账号额度。只有你主动使用 AI 功能时才调用，不会在后台自动消耗') : t('还没有可用的 AI：装好 Claude Code，或填一个自定义模型')}</small>
          </span>
          <div className="seg">
            <button
              className={backend === 'claude_cli' ? 'on' : ''}
              disabled={!s?.ai_backends.includes('claude_cli')}
              title={s?.ai_backends.includes('claude_cli') ? undefined : t('没有找到 Claude Code，安装并登录后可选')}
              onClick={() => api.aiBackendSet('claude_cli').then(reload)}
            >
              {t('本机 Claude Code')}
            </button>
            <button
              className={backend === 'api_key' ? 'on' : ''}
              disabled={!s}
              onClick={() => (s!.ai_backends.includes('api_key') ? api.aiBackendSet('api_key').then(reload) : setEditApi(true))}
            >
              {t('自定义模型')}
            </button>
          </div>
        </div>
        {s && backend === 'claude_cli' && <ClaudeForm s={s} onSaved={reload} />}
        {s?.api && backend === 'api_key' && (
          <div className="row">
            <span className="grow">
              <span>{s.api.model}</span>
              <small>{s.api.base_url}</small>
            </span>
            <button className="btn gray sm" onClick={() => setEditApi(true)}>
              {t('修改')}
            </button>
          </div>
        )}
        <div className="row">
          <span className="grow">
            <span>{t('AI 隐私授权')}</span>
            <small>{s?.ai_consent ? t('你已同意发送条目的路径、大小和进程名称等信息；不发送文件内容') : t('还没有同意过。第一次使用 AI 时会询问')}</small>
          </span>
          {s?.ai_consent && (
            <button className="btn gray sm" onClick={() => api.aiConsentRevoke().then(reload)}>
              {t('取消授权')}
            </button>
          )}
        </div>
      </div>
      <Ignored active={active} />
      {s && editApi && (
        <ApiDialog
          s={s}
          onClose={() => {
            setEditApi(false);
            reload();
          }}
        />
      )}
    </div>
  );
}
