import type { Book, Chapter, Progress } from '../types';
import type { AxiosInstance } from 'axios';

/** Scraped metadata that the book database can persist and return for review. */
export const BOOK_SCRAPE_FIELDS = [
  'title', 'author', 'narrator', 'cover_url', 'description', 'tags', 'genre', 'year',
] as const satisfies readonly (keyof Book)[];

export type ChapterGroupOrder = 'asc' | 'desc';
export interface ChapterGroupOrderEntry { book_id: string; order: ChapterGroupOrder }
export interface RegexGenerationResult {
  regex?: string;
  captured_index?: string;
  captured_title?: string;
}
export const readChapterGroupOrders = (settings: Record<string, unknown>): ChapterGroupOrderEntry[] => {
  const raw = settings.chapter_group_orders;
  const entries = Array.isArray(raw) ? raw : raw && typeof raw === 'object'
    ? Object.entries(raw).map(([book_id, order]) => ({ book_id, order })) : [];
  return entries.flatMap((entry: unknown) => {
    if (!entry || typeof entry !== 'object') return [];
    const value = entry as Record<string, unknown>;
    return typeof value.book_id === 'string' && value.book_id
      && (value.order === 'asc' || value.order === 'desc')
      ? [{ book_id: value.book_id, order: value.order }] : [];
  });
};
export const findChapterGroupOrder = (orders: ChapterGroupOrderEntry[], id: string) =>
  orders.find(entry => entry.book_id === id)?.order ?? null;
export const upsertChapterGroupOrder = (orders: ChapterGroupOrderEntry[], id: string,
  order: ChapterGroupOrder): ChapterGroupOrderEntry[] =>
  [...orders.filter(entry => entry.book_id !== id), { book_id: id, order }];

/** Shared book mutations, cancellation and metadata-save rules. The view
 * decides when to confirm; the owner does not retain dialogs or DOM state. */
export class BookOperations {
  private readonly api: Pick<AxiosInstance, 'get' | 'post' | 'patch' | 'delete'>;
  private readonly requests = new Set<AbortController>();
  private disposed = false;
  constructor(api: Pick<AxiosInstance, 'get' | 'post' | 'patch' | 'delete'>) { this.api = api; }

  async run<T>(operation: (signal: AbortSignal) => Promise<T>, parent?: AbortSignal): Promise<T> {
    if (this.disposed) throw new Error('Book operation owner disposed');
    const request = new AbortController();
    const cancel = () => request.abort();
    parent?.addEventListener('abort', cancel, { once: true });
    if (parent?.aborted) request.abort();
    this.requests.add(request);
    try {
      request.signal.throwIfAborted();
      const result = await operation(request.signal);
      if (this.disposed || request.signal.aborted) throw new Error('Book operation cancelled');
      return result;
    } finally {
      parent?.removeEventListener('abort', cancel);
      this.requests.delete(request);
    }
  }

  details(id: string, parent?: AbortSignal) {
    return this.run(async (signal) => {
      const [book, progress, settings] = await Promise.all([
        this.api.get<Book>(`/api/books/${encodeURIComponent(id)}`, { signal }),
        this.api.get<Progress>(`/api/progress/${encodeURIComponent(id)}`, { signal })
          .then(response => response.data).catch(() => null),
        this.api.get<{ settings_json?: Record<string, unknown> }>('/api/settings', { signal }),
      ]);
      return { book: book.data, progress, settings: settings.data.settings_json || {} };
    }, parent);
  }

  book(id: string) {
    return this.run(async signal =>
      (await this.api.get<Book>(`/api/books/${encodeURIComponent(id)}`, { signal })).data);
  }

  rescan(libraryId: string) {
    return this.run(signal =>
      this.api.post(`/api/libraries/${encodeURIComponent(libraryId)}/scan`, null, { signal }));
  }

  /** Rescan only these books; the rest of the library keeps its state. */
  rescanBooks(libraryId: string, bookIds: string[]) {
    return this.run(signal =>
      this.api.post(`/api/libraries/${encodeURIComponent(libraryId)}/scan`,
        { mode: 'incremental', book_ids: bookIds }, { signal }));
  }

  deleteSeries(ids: string[]) {
    return this.run(async signal => {
      for (const id of new Set(ids)) {
        signal.throwIfAborted();
        await this.api.delete(`/api/v1/series/${encodeURIComponent(id)}`, { signal });
      }
    });
  }

  favorite(id: string, value: boolean) {
    return this.run(signal => value
      ? this.api.post(`/api/favorites/${encodeURIComponent(id)}`, null, { signal })
      : this.api.delete(`/api/favorites/${encodeURIComponent(id)}`, { signal }));
  }

  deleteBooks(ids: string[], deleteFiles = false) {
    return this.run(async signal => {
      const unique = [...new Set(ids)];
      for (let offset = 0; offset < unique.length; offset += 4) {
        signal.throwIfAborted();
        const results = await Promise.allSettled(unique.slice(offset, offset + 4).map(id =>
          this.api.delete(`/api/books/${encodeURIComponent(id)}`,
            { params: { delete_files: deleteFiles }, signal })));
        const failure = results.find(result => result.status === 'rejected');
        if (failure?.status === 'rejected') throw failure.reason;
      }
    });
  }

  markRead(ids: string[], read: boolean) {
    return this.run(async signal => {
      const unique = [...new Set(ids)];
      for (let offset = 0; offset < unique.length; offset += 200) {
        signal.throwIfAborted();
        await this.api.post('/api/books/read-status',
          { book_ids: unique.slice(offset, offset + 200), read }, { signal });
      }
    });
  }

  generateRegex(filename: string, number: string, title: string) {
    return this.run(async signal => (await this.api.post<RegexGenerationResult>(
      '/api/tools/regex/generate',
      { filename, chapter_number: number, chapter_title: title }, { signal })).data);
  }

  writeMetadata(id: string) {
    return this.run(signal => this.api.post(`/api/books/${encodeURIComponent(id)}/write-metadata`, null, { signal }));
  }

  save(book: Book, metadata: Partial<Book>, cover?: File | null): Promise<Book> {
    return this.run(async signal => {
      const path = `/api/books/${encodeURIComponent(book.id)}`;
      const body = cover ? new FormData() : null;
      if (body && cover) {
        body.append('file', cover);
        body.append('metadata', JSON.stringify(metadata));
      }
      const response = body
        ? await this.api.post<Partial<Book>>(`${path}/cover`, body, { signal,
            headers: { 'Content-Type': 'multipart/form-data' } })
        : await this.api.patch<Partial<Book>>(path, metadata, { signal });
      return { ...book, ...response.data, library_type: book.library_type, is_favorite: book.is_favorite };
    });
  }

  saveGroupOrder(orders: ChapterGroupOrderEntry[], id: string, order: ChapterGroupOrder) {
    return this.run(async signal => {
      const next = upsertChapterGroupOrder(orders, id, order);
      await this.api.post('/api/settings', { chapter_group_orders: next }, { signal });
      return next;
    });
  }

  dispose() {
    this.disposed = true;
    for (const request of this.requests) request.abort();
    this.requests.clear();
  }
}

export interface ChapterPage {
  chapters: Chapter[];
  total: number;
  main_total: number;
  extra_total: number;
  chapter_type: 'main' | 'extra';
  offset: number;
}

export type ChapterTransport = (
  params: Record<string, unknown> | undefined,
  signal: AbortSignal,
) => Promise<Chapter[] | ChapterPage>;

/** One book's chapter requests and shared playback/editor catalogue.
 * Paging replaces the previous request; full-catalogue callers share one
 * request. Invalidation and disposal prevent late responses from being used.
 */
export class ChapterCatalogue {
  private readonly transport: ChapterTransport;
  private pageRequest: AbortController | null = null;
  private allRequest: AbortController | null = null;
  private allPending: Promise<Chapter[] | null> | null = null;
  private cached: Chapter[] | null = null;
  private disposed = false;

  constructor(transport: ChapterTransport) {
    this.transport = transport;
  }

  async page(params: Record<string, unknown>): Promise<ChapterPage | null> {
    if (this.disposed) return null;
    this.pageRequest?.abort();
    const request = new AbortController();
    this.pageRequest = request;
    try {
      const result = await this.transport(params, request.signal);
      if (request.signal.aborted || this.disposed) return null;
      if (Array.isArray(result)) throw new Error('Expected a chapter page');
      return result;
    } catch (error) {
      if (request.signal.aborted || this.disposed) return null;
      throw error;
    } finally {
      if (this.pageRequest === request) this.pageRequest = null;
    }
  }

  all(): Promise<Chapter[] | null> {
    if (this.disposed) return Promise.resolve(null);
    if (this.cached) return Promise.resolve(this.cached);
    if (this.allPending) return this.allPending;
    const request = new AbortController();
    this.allRequest = request;
    const pending = this.transport(undefined, request.signal).then((result) => {
      if (request.signal.aborted || this.disposed) return null;
      if (!Array.isArray(result)) throw new Error('Expected a chapter catalogue');
      this.cached = result;
      return result;
    }).catch((error: unknown) => {
      if (request.signal.aborted || this.disposed) return null;
      throw error;
    }).finally(() => {
      if (this.allPending === pending) {
        this.allPending = null;
        this.allRequest = null;
      }
    });
    this.allPending = pending;
    return pending;
  }

  invalidate() {
    this.pageRequest?.abort();
    this.allRequest?.abort();
    this.pageRequest = null;
    this.allRequest = null;
    this.allPending = null;
    this.cached = null;
  }

  dispose() {
    this.disposed = true;
    this.invalidate();
  }
}
