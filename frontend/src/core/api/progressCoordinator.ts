export interface ProgressSnapshot {
  book_id: string; chapter_id?: string | null; position: number; duration?: number;
  revision?: number; updated_at?: string;
}
export interface ProgressDraft extends ProgressSnapshot { playback_start?: number }
export interface ProgressConflict { bookId: string; local: ProgressDraft; remote: ProgressSnapshot }
export class ProgressConflictError extends Error {
  readonly current: ProgressSnapshot;
  constructor(current: ProgressSnapshot) { super('PROGRESS_CONFLICT'); this.current=current; }
}
interface Transport {
  read(book: string): Promise<ProgressSnapshot>;
  write(value: ProgressDraft, revision?: number): Promise<ProgressSnapshot>;
}
interface Entry { latest?: ProgressDraft; acknowledged?: ProgressSnapshot; conflict?: ProgressConflict; active?: Promise<void> }

export class ProgressCoordinator {
  private entries = new Map<string, Entry>();
  private transport: Transport;
  private owner: () => string;
  private onConflict: (conflict: ProgressConflict) => void;
  constructor(transport: Transport, owner: () => string, onConflict: (conflict: ProgressConflict) => void) {
    this.transport=transport; this.owner=owner; this.onConflict=onConflict;
  }
  private entry(book: string) {
    const key = `${this.owner()}|${book}`;
    let entry = this.entries.get(key);
    if (!entry) { entry = {}; this.entries.set(key, entry); }
    return entry;
  }
  hasConflict(book: string) { return !!this.entry(book).conflict; }
  save(value: ProgressDraft): Promise<void> {
    const entry = this.entry(value.book_id);
    entry.latest = value;
    if (entry.conflict) return Promise.resolve();
    return entry.active ??= this.drain(entry, this.owner()).finally(() => { entry.active = undefined; });
  }
  private setConflict(entry: Entry, local: ProgressDraft, remote: ProgressSnapshot) {
    entry.conflict = { bookId: local.book_id, local, remote };
    this.onConflict(entry.conflict);
  }
  private async drain(entry: Entry, owner: string): Promise<void> {
    while (entry.latest && !entry.conflict && owner === this.owner()) {
      const value = entry.latest;
      const remote = await this.transport.read(value.book_id);
      if (owner !== this.owner()) return;
      if (entry.latest !== value) continue;
      if (entry.acknowledged?.revision != null && remote.revision != null &&
          entry.acknowledged.revision !== remote.revision &&
          (remote.chapter_id !== value.chapter_id || Math.abs(remote.position - value.position) >= 1)) {
        this.setConflict(entry, value, remote); return;
      }
      try {
        const saved = await this.transport.write(value, remote.revision);
        if (owner !== this.owner()) return;
        entry.acknowledged = saved;
        if (entry.latest === value) entry.latest = undefined;
      } catch (error) {
        if (owner !== this.owner()) return;
        if (error instanceof ProgressConflictError) { this.setConflict(entry, entry.latest ?? value, error.current); return; }
        throw error;
      }
    }
  }
  async resolve(book: string, useRemote: boolean): Promise<ProgressSnapshot | null> {
    const owner = this.owner(); const entry = this.entry(book);
    await entry.active?.catch(()=>{});
    const current = await this.transport.read(book);
    if (owner !== this.owner()) return null;
    entry.acknowledged = current; entry.conflict = undefined;
    if (useRemote) { entry.latest = undefined; return current; }
    if (entry.latest) await this.save(entry.latest);
    return null;
  }
}
