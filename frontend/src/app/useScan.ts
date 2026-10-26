import { useCallback, useEffect, useState } from 'react';
import { api, type Progress, type ScanReport } from '../api';

export type ScanState =
  | { phase: 'idle' }
  | { phase: 'run'; progress: Progress | null }
  /** update：清理后改过的报告（去掉了已删条目、或只重扫了个别类别），不是一次完整的新扫描 */
  | { phase: 'done'; report: ScanReport; update?: boolean };

/** 扫描的状态。智能扫描页和清理空间页共用同一份。 */
export function useScan() {
  const [state, setState] = useState<ScanState>({ phase: 'idle' });

  useEffect(() => {
    api.scanLatest().then((r) => r && setState((s) => (s.phase === 'idle' ? { phase: 'done', report: r } : s)));
    const offProgress = api.onScanProgress((progress) => setState({ phase: 'run', progress }));
    const offDone = api.onScanDone((report) => setState({ phase: 'done', report }));
    // 取消后不留结果：有过结果的页面照旧显示上一份，没有的回到首页的大按钮
    const offCancelled = api.onScanCancelled(() => setState({ phase: 'idle' }));
    const offUpdate = api.onScanUpdate((report) => setState({ phase: 'done', report, update: true }));
    return () => {
      offProgress();
      offDone();
      offCancelled();
      offUpdate();
    };
  }, []);

  const start = useCallback(() => {
    setState({ phase: 'run', progress: null });
    // 后端还有一次扫描没结束（比如刚取消、还在收尾）时会拒绝；这时还没收到任何进度就退回去，让用户再点
    api.scanStart().then((ok) => {
      if (!ok) setState((s) => (s.phase === 'run' && !s.progress ? { phase: 'idle' } : s));
    });
  }, []);

  const cancel = useCallback(() => {
    api.scanCancel();
  }, []);

  // 回到还没扫描过的样子（首页大按钮），不开始扫描
  const reset = useCallback(() => setState({ phase: 'idle' }), []);

  return { state, start, cancel, reset };
}
