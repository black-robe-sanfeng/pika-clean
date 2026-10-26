import { useEffect, useRef, useState } from 'react';
import { type AiBackend, type AiError, aiCancel, api, type OnDelta, type SettingsView } from '../api';
import { t } from '../lib/i18n.ts';
import { ApiDialog } from './ApiDialog';

/** 任何带文字的 AI 结果：普通回答，或带着清理方案的回答。 */
type Result = { text: string; cached: boolean; backend: AiBackend };
/** partial 是回答已经生成出来的部分，边生成边显示；出错时也留着，不把用户已经看到的字清掉 */
type State<T> = { s: 'loading'; partial: string } | { s: 'ok'; answer: T } | { s: 'consent' } | { s: 'error'; message: string; partial: string; noBackend?: boolean };

const ERRORS: Record<string, string> = {
  no_backend: t('还没有可用的 AI。填一个自定义模型，或安装并登录 Claude Code。'),
  not_logged_in: t('AI 服务拒绝了访问：Claude Code 还没有登录，或自定义模型的 API Key 无效。'),
  timeout: t('AI 超过 60 秒没有回应。'),
  bad_output: t('AI 的回答无法识别。'),
};

const BACKEND = { claude_cli: 'Claude Code', api_key: t('自定义模型') } as const;

/**
 * 一次 AI 提问的全部状态：等待、首次使用的告知、回答、出错重试。
 * 三个 AI 入口都用它。给它一个 key，问题变了就换一个新的实例。
 */
export function AiAnswer<T extends Result>({
  ask,
  caution = t('任何删除都需要你确认'),
  onAnswer,
}: {
  /** fresh 为真表示用户点了「重新回答」，不要用缓存 */
  ask: (onDelta: OnDelta, fresh: boolean) => Promise<T>;
  caution?: string;
  /** 拿到回答后通知调用方（例如按方案勾选类别） */
  onAnswer?: (answer: T) => void;
}) {
  const [state, setState] = useState<State<T>>({ s: 'loading', partial: '' });
  // 没有可用的 AI 时就地弹出自定义模型的框，不用让用户自己去设置页找
  const [setup, setSetup] = useState<SettingsView | null>(null);
  // 每次提问一个编号：重试之后，上一次提问迟到的片段不能混进来
  const runId = useRef(0);
  const onAnswerRef = useRef(onAnswer);
  onAnswerRef.current = onAnswer;
  const askRef = useRef(ask);
  askRef.current = ask;

  // 还在生成的那次提问；界面不要它了（卸载、重新提问）就叫后端停下，免得白白跑完
  const pending = useRef<OnDelta | null>(null);
  const stop = () => {
    if (pending.current) aiCancel(pending.current);
    pending.current = null;
  };

  const run = (fresh = false) => {
    stop();
    const id = ++runId.current;
    setState({ s: 'loading', partial: '' });
    const onDelta: OnDelta = (chunk) =>
      setState((prev) => (runId.current === id && prev.s === 'loading' ? { s: 'loading', partial: prev.partial + chunk } : prev));
    pending.current = onDelta;
    const settled = () => {
      if (pending.current === onDelta) pending.current = null;
      return runId.current === id;
    };
    askRef.current(onDelta, fresh).then(
      (answer) => {
        if (!settled()) return;
        setState({ s: 'ok', answer });
        onAnswerRef.current?.(answer);
      },
      (e: AiError) => {
        if (!settled()) return;
        setState((prev) =>
          e?.kind === 'consent_required'
            ? { s: 'consent' }
            : { s: 'error', message: ERRORS[e?.kind] ?? e?.message ?? t('AI 调用失败。'), partial: prev.s === 'loading' ? prev.partial : '', noBackend: e?.kind === 'no_backend' },
        );
      },
    );
  };
  useEffect(() => {
    run();
    return stop;
  }, []);

  if (state.s === 'loading') {
    return (
      <div className="ai-box">
        {state.partial && <span>{state.partial}</span>}
        <div className="ai-st">
          <span className="spin" />
          {state.partial ? t('正在生成…') : t('AI 正在思考，通常需要十几秒…')}
        </div>
      </div>
    );
  }
  if (state.s === 'consent') {
    return (
      <div className="ai-box">
        <span>
          {t('首次使用前请知悉：会把条目的路径、大小、类别和上次使用时间，或进程的名称与占用，发给你选用的 AI 服务（本机 Claude Code 所配置的模型，或你在设置里自填的服务）。不会发送任何文件内容。')}
        </span>
        <button className="btn sm" onClick={() => api.aiConsent().then(() => run())}>
          {t('同意并继续')}
        </button>
      </div>
    );
  }
  if (state.s === 'error') {
    return (
      <div className="ai-box">
        {state.partial && <span>{state.partial}</span>}
        <span>{state.message}</span>
        {state.noBackend && (
          <button className="btn sm" onClick={() => api.settingsGet().then(setSetup)}>
            {t('填自定义模型')}
          </button>
        )}
        <button className="btn gray sm" onClick={() => run()}>
          {t('重试')}
        </button>
        {setup && (
          <ApiDialog
            s={setup}
            onClose={() => {
              setSetup(null);
              run();
            }}
          />
        )}
      </div>
    );
  }
  return (
    <div className="ai-box">
      <span>{state.answer.text}</span>
      <small>{t('{0}{1} · AI 只给建议，{2}', BACKEND[state.answer.backend], state.answer.cached ? t(' · 上次的回答') : '', caution)}</small>
      {/* 只有拿的是上次的回答时才给：点了才调用 AI，会再用一次你的额度 */}
      {state.answer.cached && (
        <button className="btn gray sm" onClick={() => run(true)}>
          {t('重新回答')}
        </button>
      )}
    </div>
  );
}
