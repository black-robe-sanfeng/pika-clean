import { useEffect, useState } from 'react';
import { api, type Category, type CategoryId, type Plan } from '../api';
import { bytes } from '../lib/fmt';
import { AiAnswer } from './AiAnswer';
import { t } from '../lib/i18n.ts';

/** 点一下就发出去的常用提问。都是能落到「勾选哪些类别」上的说法，因为 AI 只能在扫描出的类别里挑。 */
const PRESETS = [t('只清不影响使用的'), t('尽量多腾空间，个人文件别动'), t('帮我腾出 10 GB'), t('只清缓存和日志')];

/**
 * 「一句话清理」：输入框、常用提问、AI 给的方案，以及把方案变成勾选的按钮。
 * AI 只挑类别；点了按钮也只是勾选并打开确认单，删不删仍由用户在确认单里决定。
 */
export function AskClean({
  categories,
  onPlan,
  onApply,
  disabled,
}: {
  /** 最近一次扫描的类别，用来算方案能腾出多少 */
  categories: Category[];
  /** 方案一出来就通知（清理页用它直接改勾选） */
  onPlan?: (ids: CategoryId[]) => void;
  /** 用户点了「按这个勾选并查看确认单」 */
  onApply: (ids: CategoryId[]) => void;
  /** 正在清理或正在扫描：报告马上要变，不让按旧方案打开确认单 */
  disabled?: boolean;
}) {
  const [wish, setWish] = useState('');
  // 用户的原话和第几次提问（用来在重新提问时换一个新的回答框）
  const [asked, setAsked] = useState<{ text: string; n: number } | null>(null);
  const [plan, setPlan] = useState<Plan | null>(null);
  // 每来一份新报告，上一次的方案就过时了
  useEffect(() => {
    setAsked(null);
    setPlan(null);
  }, [categories]);

  const ask = (text: string) => {
    setWish(text);
    setPlan(null);
    setAsked({ text, n: (asked?.n ?? 0) + 1 });
  };
  const planBytes = categories.filter((c) => plan?.categories.includes(c.id)).reduce((s, c) => s + c.total_bytes, 0);

  return (
    <div className="ask">
      <form
        className="askline"
        onSubmit={(e) => {
          e.preventDefault();
          if (wish.trim()) ask(wish.trim());
        }}
      >
        <input className="search" value={wish} onChange={(e) => setWish(e.target.value)} aria-label={t('用一句话告诉 AI 你想怎么清理')} placeholder={t('让 AI 帮你勾选，例如：帮我腾出空间，别动下载的文件')} />
        <button className="btn gray" disabled={!wish.trim()}>
          {t('让 AI 勾选')}
        </button>
      </form>
      <div className="asks">
        {PRESETS.map((q) => (
          <button key={q} onClick={() => ask(q)}>
            {q}
          </button>
        ))}
      </div>
      {asked && (
        <AiAnswer
          key={asked.n}
          ask={(_, fresh) => api.aiPlan(asked.text, fresh).then((p) => ({ ...p, text: p.reason || (p.categories.length ? t('已按你的要求挑好了类别。') : t('没有找到符合要求的类别。')) }))}
          onAnswer={(p) => {
            setPlan(p);
            onPlan?.(p.categories);
          }}
          caution={t('清理前还要你在确认单里确认')}
        />
      )}
      {plan && plan.categories.length > 0 && (
        <button className="btn" disabled={disabled} onClick={() => onApply(plan.categories)}>{t('按这个勾选并查看确认单 · {0} 类 {1}', plan.categories.length, bytes(planBytes))}</button>
      )}
    </div>
  );
}
