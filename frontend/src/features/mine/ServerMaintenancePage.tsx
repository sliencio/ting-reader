import { useEffect, useState } from 'react';
import { Link } from 'react-router';
import api from '../../core/api/client';
import { apiErrorMessage } from '../../core/api/errors';

interface Settings {
  automatic_backup: boolean; backup_interval_hours: number; backup_retention: number;
  preload_window_mib: number; preload_memory_mib: number; preload_idle_seconds: number;
  disk_cache_chapters: number; disk_cache_mib: number | null;
}
interface Backup { name: string; bytes: number; created_at: string }
interface Backups { files: Backup[]; pending_restore: boolean; last_error?: string }
interface Library { id: string; name: string }
interface Report {
  database_ms: number;
  directories: { name: string; writable: boolean; free_bytes?: number; error?: string }[];
  tools: { name: string; available: boolean; version?: string; error?: string }[];
  cache: { files: number; disk_bytes: number; memory: { bytes: number; hits: number; misses: number } };
  last_backup_error?: string;
}
const bytes = (value: number) => `${(value / 1024 / 1024).toFixed(1)} MiB`;
const button = 'rounded-xl px-4 py-2 text-sm font-medium bg-slate-100 dark:bg-slate-800 disabled:opacity-40';
const input = 'w-full rounded-xl border border-slate-200 dark:border-slate-700 bg-transparent px-3 py-2';

export default function ServerMaintenancePage() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [backups, setBackups] = useState<Backups>({ files: [], pending_restore: false });
  const [libraries, setLibraries] = useState<Library[]>([]);
  const [report, setReport] = useState<Report | null>(null);
  const [libraryReports, setLibraryReports] = useState<Record<string, string>>({});
  const [tab, setTab] = useState('backup');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const [restore, setRestore] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState('');

  const refresh = async () => {
    const [configuration, saved, sources] = await Promise.all([
      api.get<Settings>('/api/system/maintenance'), api.get<Backups>('/api/system/backups'),
      api.get<Library[]>('/api/libraries'),
    ]);
    setSettings(configuration.data); setBackups(saved.data); setLibraries(sources.data);
  };
  useEffect(() => {
    let mounted = true;
    void Promise.all([api.get<Settings>('/api/system/maintenance'), api.get<Backups>('/api/system/backups'), api.get<Library[]>('/api/libraries')])
      .then(([configuration, saved, sources]) => {
        if (mounted) { setSettings(configuration.data); setBackups(saved.data); setLibraries(sources.data); }
      }).catch((error: unknown) => { if (mounted) setMessage(apiErrorMessage(error, '读取维护设置失败，请升级服务端或重试')); });
    return () => { mounted = false; };
  }, []);
  const run = async (action: () => Promise<void>, success = '') => {
    if (busy) return;
    setBusy(true); setMessage('');
    try { await action(); if (success) setMessage(success); }
    catch (error) { setMessage(apiErrorMessage(error, '操作失败，请重试')); }
    finally { setBusy(false); }
  };
  const save = () => run(async () => {
    await api.put('/api/system/maintenance', settings); await refresh();
  }, '设置已保存');
  const numberField = (key: Exclude<keyof Settings, 'automatic_backup'>, label: string, min: number, max: number) => (
    <label className="space-y-2 block" key={key}>
      <span className="text-sm">{label}</span>
      <input type="number" className={input} min={min} max={max} disabled={busy}
        value={settings?.[key] ?? ''}
        onChange={event => setSettings(current => current && ({ ...current, [key]: event.target.value === '' && key === 'disk_cache_mib' ? null : Number(event.target.value) }))} />
    </label>
  );
  const download = (file: Backup) => run(async () => {
    const response = await api.get<Blob>(`/api/system/backups/${encodeURIComponent(file.name)}`, { responseType: 'blob', timeout: 120000 });
    const url = URL.createObjectURL(response.data);
    const link = document.createElement('a'); link.href = url; link.download = file.name; link.click();
    setTimeout(() => URL.revokeObjectURL(url), 30000);
  });

  return <div className="max-w-5xl mx-auto px-4 py-6 space-y-6">
    <header className="flex flex-wrap items-center justify-between gap-3">
      <div><Link to="/mine" className="text-sm text-slate-500">返回设置</Link><h1 className="text-2xl font-bold mt-2">服务端维护</h1></div>
      <button className={button} disabled={busy} onClick={() => void run(refresh)}>刷新</button>
    </header>
    <div className="flex flex-wrap gap-2" role="tablist" aria-label="维护项目">
      {[['backup', '备份与恢复'], ['cache', '预加载与缓存'], ['diagnostics', '故障诊断']].map(([key, label]) =>
        <button key={key} role="tab" aria-selected={tab === key} className={`${button} ${tab === key ? 'ring-2 ring-primary-500' : ''}`} onClick={() => setTab(key)}>{label}</button>)}
    </div>
    {message && <p role="status" className="rounded-xl bg-sky-50 dark:bg-slate-800 p-4 break-words">{message}</p>}
    {!settings ? <p>服务端需支持维护接口。加载失败时请先升级服务端，再点击刷新。</p> : <>
      {tab === 'backup' && <div className="space-y-5">
        <section className="rounded-2xl border border-slate-200 dark:border-slate-800 p-5 space-y-4">
          <h2 className="text-lg font-semibold">自动备份</h2>
          <p className="text-sm text-slate-500">备份包含账号、阅读状态和数据库内设置，不包含音频及外部配置。跨机器或变更数据库路径恢复后，可能需重新填写 WebDAV、插件凭据。升级前另外保存备份；自动清理只处理自动备份。</p>
          <label className="flex gap-3 items-center"><input type="checkbox" checked={settings.automatic_backup} disabled={busy} onChange={event => setSettings({ ...settings, automatic_backup: event.target.checked })} />启用定时备份</label>
          <div className="grid sm:grid-cols-2 gap-4">
            {numberField('backup_interval_hours', '备份间隔（小时，1–168）', 1, 168)}
            {numberField('backup_retention', '自动备份保留份数（1–60）', 1, 60)}
          </div>
          <button className={button} disabled={busy} onClick={() => void save()}>保存设置</button>
        </section>
        {backups.pending_restore && <div className="rounded-xl bg-amber-50 dark:bg-amber-950 p-4 space-y-3">
          <p>已暂存恢复请求。请重启服务端应用恢复，届时会先备份当前数据库；恢复后可能需要重新登录。</p>
          <button className={button} disabled={busy} onClick={() => void run(async () => { await api.delete('/api/system/backups/pending-restore'); await refresh(); }, '已取消恢复')}>取消待恢复请求</button>
        </div>}
        {backups.last_error && <p className="text-red-500 break-words">最近自动备份失败：{backups.last_error}</p>}
        <div className="flex flex-wrap items-center gap-3">
          <button className={button} disabled={busy} onClick={() => void run(async () => { await api.post('/api/system/backups', {}, { timeout: 120000 }); await refresh(); }, '备份已创建')}>立即备份</button>
          <label className={`${button} cursor-pointer`}>导入数据库备份
            <input type="file" accept=".db,.sqlite,.sqlite3" className="sr-only" disabled={busy} onChange={event => {
              const file = event.target.files?.[0]; event.target.value = '';
              if (!file) return;
              if (file.size > 512 * 1024 * 1024) { setMessage('备份不能超过 512 MiB'); return; }
              const body = new FormData(); body.append('backup', file);
              void run(async () => { await api.post('/api/system/backups/import', body, { headers: { 'Content-Type': 'multipart/form-data' }, timeout: 120000 }); await refresh(); }, '导入并校验成功，尚未恢复');
            }} />
          </label>
        </div>
        {!backups.files.length && <p className="text-slate-500">暂无备份</p>}
        {backups.files.map(file => <article key={file.name} className="rounded-2xl border border-slate-200 dark:border-slate-800 p-4 flex flex-wrap justify-between items-center gap-3">
          <div className="min-w-0"><p className="break-all text-sm font-medium">{file.name}</p><p className="text-sm text-slate-500">{bytes(file.bytes)} · {new Date(file.created_at).toLocaleString()}</p></div>
          <div className="flex gap-2 flex-wrap">
            <button className={button} disabled={busy} onClick={() => void download(file)}>下载</button>
            <button className={button} disabled={busy || backups.pending_restore} onClick={() => { setRestore(file.name); setConfirmation(''); }}>恢复</button>
            <button className={button} disabled={busy} onClick={() => {
              if (window.confirm(`删除备份 ${file.name}？`)) void run(async () => { await api.delete(`/api/system/backups/${encodeURIComponent(file.name)}`); await refresh(); });
            }}>删除</button>
          </div>
        </article>)}
      </div>}
      {tab === 'cache' && <section className="rounded-2xl border border-slate-200 dark:border-slate-800 p-5 space-y-5">
        <h2 className="text-lg font-semibold">服务端预加载与缓存策略</h2>
        <p className="text-sm text-slate-500">用于服务端的下一章预加载，与手机或电脑的下载缓存独立。是否自动预加载继续遵循账号的播放设置。</p>
        <div className="grid sm:grid-cols-2 gap-4">
          {numberField('preload_window_mib', '每章预加载大小（MiB，1–16）', 1, 16)}
          {numberField('preload_memory_mib', '预加载内存上限（MiB，4–256）', 4, 256)}
          {numberField('preload_idle_seconds', '预加载空闲有效期（秒，5–3600）', 5, 3600)}
          {numberField('disk_cache_chapters', '磁盘缓存章节上限（1–500）', 1, 500)}
          {numberField('disk_cache_mib', '磁盘缓存上限（MiB，留空沿用原配置）', 64, 65536)}
        </div>
        <button className={button} disabled={busy} onClick={() => void save()}>保存设置</button>
        <Link className="ml-4 text-primary-600 text-sm" to="/cache">管理已有音频缓存</Link>
      </section>}
      {tab === 'diagnostics' && <div className="space-y-5">
        <button className={button} disabled={busy} onClick={() => void run(async () => setReport((await api.get<Report>('/api/system/diagnostics', { timeout: 60000 })).data))}>运行服务端检查</button>
        {report && <section className="rounded-2xl border border-slate-200 dark:border-slate-800 p-5 space-y-4">
          <p>数据库响应：{report.database_ms} ms</p>
          {report.directories.map(item => <p key={item.name}>{item.name}：{item.writable ? '可写' : '不可写'}{item.free_bytes != null && `，可用 ${bytes(item.free_bytes)}`}{item.error && `，${item.error}`}</p>)}
          {report.tools.map(item => <p key={item.name} className="break-words">{item.name}：{item.available ? item.version : item.error}</p>)}
          <p>磁盘缓存 {report.cache.files} 章 / {bytes(report.cache.disk_bytes)}；预加载内存 {bytes(report.cache.memory.bytes)}</p>
          <p>本次运行预加载命中 {report.cache.memory.hits} 次，未命中 {report.cache.memory.misses} 次</p>
          {report.last_backup_error && <p className="text-red-500">自动备份：{report.last_backup_error}</p>}
        </section>}
        {libraries.map(library => <section key={library.id} className="rounded-2xl border border-slate-200 dark:border-slate-800 p-4 space-y-3">
          <div className="flex flex-wrap justify-between items-center gap-3"><h2 className="font-semibold">{library.name}</h2><button className={button} disabled={busy} onClick={() => void run(async () => {
            const { data } = await api.post<{ elapsed_ms: number; result?: { message: string }; error?: string }>(`/api/system/diagnostics/libraries/${encodeURIComponent(library.id)}`, {}, { timeout: 20000 });
            setLibraryReports(current => ({ ...current, [library.id]: `${data.error ?? data.result?.message ?? '检查完成'}（${data.elapsed_ms} ms）` }));
          })}>检查连接及抽样章节</button></div>
          {libraryReports[library.id] && <p className="text-sm break-words">{libraryReports[library.id]}</p>}
        </section>)}
      </div>}
    </>}
    {restore && <div className="fixed inset-0 z-50 bg-black/40 flex items-center justify-center p-4" role="dialog" aria-modal="true" aria-label="恢复备份">
      <div className="bg-white dark:bg-slate-900 rounded-2xl p-6 max-w-lg w-full space-y-4 max-h-[85vh] overflow-y-auto">
        <h2 className="text-lg font-bold">确认暂存恢复请求</h2><p className="text-sm">恢复会在下次重启时替换数据库中的账号、进度与设置，不会替换音频文件。重启前可以取消。</p>
        <p className="text-sm break-all">{restore}</p><label className="block">请输入“恢复备份”<input autoFocus className={`${input} mt-2`} value={confirmation} onChange={event => setConfirmation(event.target.value)} /></label>
        <div className="flex flex-wrap gap-3"><button className={button} disabled={busy} onClick={() => setRestore(null)}>取消</button><button className={button} disabled={busy || confirmation !== '恢复备份'} onClick={() => void run(async () => {
          await api.post(`/api/system/backups/${encodeURIComponent(restore)}/restore`, { confirmation }); setRestore(null); await refresh();
        }, '已暂存恢复请求，请重启服务端')}>确认暂存恢复</button></div>
      </div>
    </div>}
  </div>;
}
