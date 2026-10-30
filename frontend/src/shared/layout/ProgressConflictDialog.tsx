import { useEffect, useState } from 'react';
import { progressCoordinator } from '../../core/api/progressSync';
import type { ProgressConflict } from '../../core/api/progressCoordinator';
import { usePlayerStore } from '../../core/stores/playerStore';
import { apiErrorMessage } from '../../core/api/errors';
import api from '../../core/api/client';
import type { Chapter } from '../../core/types';

export default function ProgressConflictDialog() {
  const [conflict, setConflict] = useState<ProgressConflict | null>(null);
  const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  useEffect(() => {
    const handler = (event: Event) => { setConflict((event as CustomEvent<ProgressConflict>).detail); setError(''); };
    window.addEventListener('ting-progress-conflict', handler);
    return () => window.removeEventListener('ting-progress-conflict', handler);
  }, []);
  if (!conflict) return null;
  const choose = async (remote: boolean) => {
    setBusy(true); setError('');
    try {
      const value = await progressCoordinator.resolve(conflict.bookId, remote);
      if (progressCoordinator.hasConflict(conflict.bookId)) return;
      if (value && remote) {
        const player = usePlayerStore.getState();
        let chapters=player.chapters;
        let chapter = chapters.find(item => item.id === value.chapter_id) ?? (value.chapter_id==null?chapters[0]:undefined);
        if (!chapter && player.currentBook?.id === conflict.bookId) {
          chapters=(await api.get<Chapter[]>(`/api/books/${encodeURIComponent(conflict.bookId)}/chapters`)).data;
          chapter=chapters.find(item=>item.id===value.chapter_id) ?? (value.chapter_id==null?chapters[0]:undefined);
        }
        if (player.currentBook?.id === conflict.bookId && chapter) {
          player.playChapter(player.currentBook, chapters, chapter, value.position);
        }
      }
      setConflict(current => current === conflict ? null : current);
    } catch (failure) { setError(apiErrorMessage(failure, '处理进度冲突失败，请重试')); }
    finally { setBusy(false); }
  };
  return <div className="fixed inset-0 z-[100] bg-black/40 flex items-center justify-center p-4" role="dialog" aria-modal="true" aria-label="进度冲突">
    <section className="bg-white dark:bg-slate-900 p-6 rounded-2xl max-w-lg w-full space-y-4">
      <h2 className="text-lg font-bold">其他设备已更新进度</h2>
      <p>请选择续播位置。选择前会保留本机进度，停止向服务端覆盖写入。</p>
      <p className="text-sm">本机：{conflict.local.chapter_id} · {Math.floor(conflict.local.position)} 秒</p>
      <p className="text-sm">远端：{conflict.remote.chapter_id ?? '未播放'} · {Math.floor(conflict.remote.position)} 秒</p>
      {error && <p role="alert" className="text-red-500 break-words">{error}</p>}
      <div className="flex flex-wrap gap-3">
        <button className="rounded-xl bg-primary-600 text-white px-4 py-2 disabled:opacity-40" disabled={busy} onClick={() => void choose(true)}>采用远端进度</button>
        <button className="rounded-xl bg-slate-100 dark:bg-slate-800 px-4 py-2 disabled:opacity-40" disabled={busy} onClick={() => void choose(false)}>使用本机进度</button>
      </div>
    </section>
  </div>;
}
