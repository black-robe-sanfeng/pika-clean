import { useEffect, useRef, useState } from 'react';
import { api, type ScanReport, type UpdateInfo } from '../api';
import { Sidebar, type ViewId, VIEWS } from '../components/Sidebar';
import { Dialog } from '../components/Dialog';
import { ToastHost } from '../components/Toast';
import { UpdateDialog } from '../components/UpdateDialog';
import { AppsPage } from '../pages/AppsPage';
import { CarePage } from '../pages/CarePage';
import { CleanPage } from '../pages/CleanPage';
import { OptimizePage } from '../pages/OptimizePage';
import { ScanPage } from '../pages/ScanPage';
import { SettingsPage } from '../pages/SettingsPage';
import { SpacePage } from '../pages/SpacePage';
import { useScan } from './useScan';
import { t } from '../lib/i18n.ts';

/** 存下来的或后端发来的页面名不认识时回到智能扫描（比如合并前的「清理空间」）。 */
const toView = (v: string | null): ViewId => VIEWS.find((x) => x === v) ?? 'scan';

export function App() {
  // 当前页面记在浏览器的会话存储里：换界面语言会整页重新加载，回来还在原来那一页。应用重启后仍从智能扫描开始
  const [view, setView] = useState<ViewId>(() => toView(sessionStorage.getItem('view')));
  useEffect(() => sessionStorage.setItem('view', view), [view]);
  const scan = useScan();
  // 智能扫描页：还没有结果时是一个大按钮，有过结果之后一直是结果列表（重新扫描时列表留着不闪）
  const report = useRef<ScanReport | null>(null);
  if (scan.state.phase === 'done') report.current = scan.state.report;
  const scanned = report.current != null;
  // 重新扫描：丢掉旧结果，回到首页的大按钮，由用户再点开始
  const rescan = () => {
    report.current = null;
    scan.reset();
  };

  // 第一次启动时说明三件用户最容易困惑的事
  const [welcome, setWelcome] = useState(false);
  useEffect(() => {
    api.settingsGet().then((s) => setWelcome(!s.onboarded));
  }, []);

  // 启动时查一次新版本，之后每 6 小时查一次：应用常驻菜单栏，可能几周都不重启。
  // 同一个版本只提示一次（关掉就不再弹，下次启动再提示）；查不到（没网、服务器没开）不打扰
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  useEffect(() => {
    const shown = new Set<string>();
    const check = () =>
      api.updateCheck().then(
        (u) => {
          if (u && !shown.has(u.version)) {
            shown.add(u.version);
            setUpdate(u);
          }
        },
        () => {},
      );
    check();
    const timer = window.setInterval(check, 6 * 3600_000);
    return () => clearInterval(timer);
  }, []);

  // 空间去向里点「去清理」「去卸载」带过去的位置：清理页打开它所在的类别，卸载页把应用名填进搜索框
  const [jump, setJump] = useState<{ view: 'scan' | 'apps'; path: string } | null>(null);

  // 菜单栏面板的「去清理」「打开主窗口」会让后端发这个事件
  useEffect(() => api.onNavigate((v) => setView(toView(v))), []);

  return (
    <>
      <div className="drag" data-tauri-drag-region />
      <div className="window">
        <Sidebar view={view} onGo={setView} />
        <main className="main">
          <ToastHost />
          <section className="view" hidden={view !== 'scan'}>
            {scanned ? <CleanPage scan={scan.state} onRescan={rescan} focus={jump?.view === 'scan' ? jump : null} /> : <ScanPage scan={scan.state} onScan={scan.start} onCancel={scan.cancel} />}
          </section>
          <section className="view" hidden={view !== 'space'}>
            <SpacePage
              active={view === 'space'}
              onGo={(v, path) => {
                setJump({ view: v, path });
                setView(v);
              }}
            />
          </section>
          <section className="view" hidden={view !== 'apps'}>
            <AppsPage active={view === 'apps'} focus={jump?.view === 'apps' ? jump : null} />
          </section>
          <section className="view" hidden={view !== 'opt'}>
            <OptimizePage active={view === 'opt'} />
          </section>
          <section className="view" hidden={view !== 'care'}>
            <CarePage active={view === 'care'} />
          </section>
          <section className="view" hidden={view !== 'settings'}>
            <SettingsPage active={view === 'settings'} />
          </section>
        </main>
      </div>
      {!welcome && update && <UpdateDialog info={update} onClose={() => setUpdate(null)} />}
      {welcome && (
        <Dialog
          title={t('欢迎使用 Pika Clean')}
          actions={
            <button
              className="btn"
              onClick={() => {
                setWelcome(false);
                api.onboardedSet();
              }}
            >
              {t('开始使用')}
            </button>
          }
        >
          <div className="group plain">
            <div className="row">
              <span className="grow" style={{ whiteSpace: 'normal' }}>
                <span>{t('它常驻在菜单栏')}</span>
                <small style={{ whiteSpace: 'normal' }}>{t('关掉这个窗口不会退出。点屏幕顶部菜单栏里的小图标可以看系统状态，右键可以退出。')}</small>
              </span>
            </div>
            <div className="row">
              <span className="grow">
                <span>{t('删除前都会先问你')}</span>
                <small style={{ whiteSpace: 'normal' }}>{t('缓存和日志直接删除；下载的文件和卸载的应用只移到废纸篓，清空前都能找回。')}</small>
              </span>
            </div>
            <div className="row">
              <span className="grow">
                <span>{t('读废纸篓需要额外授权')}</span>
                <small style={{ whiteSpace: 'normal' }}>{t('不授权也能用其余功能。需要时在「设置」里点「去授权」。')}</small>
              </span>
            </div>
          </div>
        </Dialog>
      )}
    </>
  );
}
