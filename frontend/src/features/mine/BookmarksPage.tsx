import { apiErrorMessage } from '../../core/api/errors';
import React, { useCallback, useEffect, useRef, useState } from 'react';
import { BookMarked, Clock, MessageSquare, Pencil, Trash2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import apiClient from '../../core/api/client';
import { playBookmark, formatBookmarkPosition, type ActivityBook, type Bookmark, type ReadingPage as Page } from '../../core/api/reading';
import { usePlayerStore } from '../../core/stores/playerStore';
import { useBookshelfCoverShape } from '../../core/hooks/useBookshelfCoverShape';
import BackButton from '../../shared/widgets/BackButton';
import LoadingSpinner from '../../shared/ui/LoadingSpinner';
import ActivityBookCard, { ActivityProgress } from './ActivityBookCard';
import { chapterProgressPercent } from '../../core/utils/playbackPreferences';
import { useServerRefresh } from '../../core/hooks/useServerChanges';


const BookmarksPage: React.FC = () => {
  const { t } = useTranslation();
  const currentChapter = usePlayerStore((state) => state.currentChapter);
  const coverShape = useBookshelfCoverShape();
  const [books, setBooks] = useState<ActivityBook[]>([]);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [bookmarks, setBookmarks] = useState<Record<string, Bookmark[]>>({});
  const [pages, setPages] = useState<Record<string, number>>({});
  const [totals, setTotals] = useState<Record<string, number>>({});
  const [loadingBooks, setLoadingBooks] = useState(true);
  const [loadingIds, setLoadingIds] = useState<Set<string>>(new Set());
  const [bookPage, setBookPage] = useState(1);
  const [totalBooks, setTotalBooks] = useState(0);
  const [playingBookmark, setPlayingBookmark] = useState(false);
  const bookRequest = useRef(0);
  const chapterRequests = useRef(new Map<string, number>());

  const fetchBooks = useCallback(async (page = 1, background = false) => {
    const request = ++bookRequest.current;
    if (!background) setLoadingBooks(true);
    try {
      const response = await apiClient.get<Page<ActivityBook>>('/api/bookmarks/books', { params: { page, page_size: 40 } });
      if (request !== bookRequest.current) return;
      setBooks(response.data.items || []);
      setBookPage(page);
      setTotalBooks(response.data.total);
      if (!background) {
        setExpanded(new Set());
        setBookmarks({});
        setPages({});
        setTotals({});
      }
      return response.data.items || [];
    } catch (error) {
      console.error('获取书签书籍失败', error);
    } finally {
      if (request === bookRequest.current) setLoadingBooks(false);
    }
  }, []);
  useEffect(() => {
    const timer = window.setTimeout(() => void fetchBooks(), 0);
    return () => window.clearTimeout(timer);
  }, [fetchBooks]);

  const loadBookmarks = async (bookId: string, page: number, background = false) => {
    const request = (chapterRequests.current.get(bookId) ?? 0) + 1;
    chapterRequests.current.set(bookId, request);
    setLoadingIds((current) => new Set(current).add(bookId));
    try {
      const response = await apiClient.get<Page<Bookmark>>(`/api/bookmarks/books/${bookId}`, { params: { page, page_size: 50 } });
      if (chapterRequests.current.get(bookId) !== request) return;
      setBookmarks((current) => ({ ...current, [bookId]: response.data.items }));
      setPages((current) => ({ ...current, [bookId]: page }));
      setTotals((current) => ({ ...current, [bookId]: response.data.total }));
      setBooks((current) => current.map((book) => book.book_id === bookId
        ? { ...book, chapter_count: response.data.total } : book));
      return response.data.total;
    } catch (error) {
      console.error('获取书签失败', error);
      if (!background) alert(apiErrorMessage(error, t('bookmarks.loadFailed', '加载书签失败')));
    } finally {
      if (chapterRequests.current.get(bookId) === request) setLoadingIds((current) => { const next = new Set(current); next.delete(bookId); return next; });
    }
  };

  useServerRefresh(async () => {
    const latest = await fetchBooks(bookPage, true);
    const ids = new Set(latest?.map(book => book.book_id));
    await Promise.all([...expanded].filter(id => ids.has(id)).map(id => loadBookmarks(id, pages[id] || 1, true)));
  });

  const toggleBook = (bookId: string) => {
    const open = expanded.has(bookId);
    setExpanded((current) => {
      const next = new Set(current);
      if (open) next.delete(bookId);
      else next.add(bookId);
      return next;
    });
    if (!open && !bookmarks[bookId]) void loadBookmarks(bookId, 1);
  };

  const editNote = async (bookmark: Bookmark) => {
    const note = window.prompt(t('bookmarks.editNote', '编辑书签备注'), bookmark.note);
    if (note === null) return;
    try {
      await apiClient.put(`/api/bookmarks/${bookmark.id}`, { note });
      await loadBookmarks(bookmark.book_id, 1);
    } catch (error) {
      console.error('更新书签失败', error);
      alert(apiErrorMessage(error, t('common.saveFailed')));
    }
  };

  const deleteBookmark = async (bookmark: Bookmark) => {
    if (!window.confirm(t('bookmarks.confirmDelete', '删除此书签？'))) return;
    try {
      await apiClient.delete(`/api/bookmarks/${bookmark.id}`);
      const remaining = await loadBookmarks(bookmark.book_id, 1);
      if (remaining === 0) {
        await fetchBooks(books.length === 1 && bookPage > 1 ? bookPage - 1 : bookPage);
      } else if (remaining !== undefined) {
        setBooks((current) => current.map((book) => book.book_id === bookmark.book_id ? { ...book, chapter_count: remaining } : book));
      }
    } catch (error) {
      console.error('删除书签失败', error);
      alert(apiErrorMessage(error, t('common.deleteFailed', '删除失败')));
    }
  };

  const jump = async (bookmark: Bookmark) => {
    if (playingBookmark) return;
    setPlayingBookmark(true);
    try { await playBookmark(bookmark); }
    catch (error) { alert(apiErrorMessage(error, t('bookmarks.playFailed', '书签播放失败'))); }
    finally { setPlayingBookmark(false); }
  };

  if (loadingBooks) return <LoadingSpinner />;
  return <div className="flex min-h-full flex-1 flex-col p-4 sm:p-6 md:p-8">
    <div className="flex-1 space-y-6">
      <BackButton fallback="/mine" />
      <header><h1 className="flex items-center gap-3 text-2xl font-bold text-slate-900 dark:text-white"><BookMarked className="text-primary-600" />{t('bookmarks.title', '我的书签')}</h1><p className="mt-1 text-sm text-slate-500">{t('bookmarks.subtitle', '按书籍查看保存的位置和备注')}</p></header>
      {books.length ? <div className="space-y-3">{books.map((book) => <ActivityBookCard
        key={book.book_id}
        book={{ ...book, book_title: book.book_title || t('historyPage.unknownBook') }}
        coverShape={coverShape}
        countLabel={t('bookmarks.count', { count: totals[book.book_id] ?? book.chapter_count })}
        expanded={expanded.has(book.book_id)}
        onExpand={() => toggleBook(book.book_id)}
      >
          {(bookmarks[book.book_id] || []).map((bookmark) => <div key={bookmark.id} className="group flex items-start gap-2 border-t border-slate-100 px-4 py-4 first:border-t-0 md:gap-3 md:px-5 dark:border-slate-800">
            <button disabled={playingBookmark} onClick={() => void jump(bookmark)} className="min-w-0 flex-1 text-left disabled:opacity-50">
              <span className="block truncate text-sm font-bold text-slate-800 transition-colors group-hover:text-primary-600 dark:text-slate-100">{bookmark.chapter_title || t('historyPage.unknownChapter')}</span>
              <span className="mt-1.5 flex items-center gap-1.5 text-xs text-slate-400"><Clock size={13} className="shrink-0" />{formatBookmarkPosition(bookmark.position)}</span>
              {bookmark.note && <span className="mt-3 flex items-start gap-2 rounded-xl bg-white/80 px-3 py-2.5 text-xs leading-relaxed text-slate-500 dark:bg-slate-900/60 dark:text-slate-400"><MessageSquare size={13} className="mt-0.5 shrink-0 text-slate-400" /><span className="line-clamp-3 whitespace-pre-wrap break-words">{bookmark.note}</span></span>}
              <ActivityProgress percent={chapterProgressPercent(bookmark.position, bookmark.chapter_duration)} />
            </button>
            <button onClick={() => void editNote(bookmark)} title={t('bookmarks.editNote', '编辑备注')} className="rounded-full p-2 text-slate-400 transition-colors hover:bg-white hover:text-primary-600 dark:hover:bg-slate-800"><Pencil size={16} /></button>
            <button onClick={() => void deleteBookmark(bookmark)} title={t('common.delete')} className="rounded-full p-2 text-slate-400 transition-colors hover:bg-red-50 hover:text-red-500 dark:hover:bg-red-950/30"><Trash2 size={16} /></button>
          </div>)}
          {loadingIds.has(book.book_id) && <div className="p-4"><LoadingSpinner /></div>}
          {(totals[book.book_id] || 0) > 50 && <div className="flex items-center justify-center gap-4 p-3 text-sm">
            <button disabled={loadingIds.has(book.book_id) || (pages[book.book_id] || 1) <= 1} onClick={() => void loadBookmarks(book.book_id, (pages[book.book_id] || 1) - 1)} className="disabled:opacity-40">{t('common.previous', '上一页')}</button>
            <span>{pages[book.book_id] || 1} / {Math.ceil(totals[book.book_id] / 50)}</span>
            <button disabled={loadingIds.has(book.book_id) || (pages[book.book_id] || 1) * 50 >= totals[book.book_id]} onClick={() => void loadBookmarks(book.book_id, (pages[book.book_id] || 1) + 1)} className="disabled:opacity-40">{t('common.next', '下一页')}</button>
          </div>}
      </ActivityBookCard>)}</div> : <div className="rounded-3xl border border-dashed border-slate-200 bg-white p-10 text-center text-sm text-slate-500 dark:border-slate-800 dark:bg-slate-900">{t('bookmarks.empty', '还没有书签')}</div>}
      {totalBooks > 40 && <div className="flex items-center justify-center gap-4 text-sm">
        <button disabled={bookPage <= 1} onClick={() => void fetchBooks(bookPage - 1)} className="disabled:opacity-40">{t('common.previous', '上一页')}</button>
        <span>{bookPage} / {Math.ceil(totalBooks / 40)}</span>
        <button disabled={bookPage * 40 >= totalBooks} onClick={() => void fetchBooks(bookPage + 1)} className="disabled:opacity-40">{t('common.next', '下一页')}</button>
      </div>}
    </div>
    <div className="shrink-0" style={{ height: currentChapter ? 'var(--safe-bottom-with-player)' : 'var(--safe-bottom-base)' }} />
  </div>;
};

export default BookmarksPage;
