import { useEffect, useEffectEvent } from 'react';
import api from '../api/client';
import { useAuthStore } from '../stores/authStore';

interface ServerState { instance: string; content_revision: number; reading_revision: number }
export interface ServerNotice { content: boolean; reading: boolean }
let previous: { owner: string; state: ServerState } | undefined;

export function acceptServerState(value: unknown) {
  const state = value as ServerState;
  if (!state || typeof state.instance !== 'string' || typeof state.content_revision !== 'number') return;
  const auth = useAuthStore.getState(); const owner = `${auth.activeUrl}|${auth.user?.id}`;
  const old = previous?.owner === owner ? previous.state : undefined;
  previous = { owner, state };
  if (!old) return;
  const content = old.instance !== state.instance || old.content_revision !== state.content_revision;
  const reading = old.instance !== state.instance || old.reading_revision !== state.reading_revision;
  if (content || reading) window.dispatchEvent(new CustomEvent<ServerNotice>('ting-server-changed', { detail: { content, reading } }));
}

export function useServerNotifications() {
  const token = useAuthStore(state => state.token);
  const activeUrl = useAuthStore(state => state.activeUrl);
  useEffect(() => {
    if (!token) return;
    let active = true; let checking = false;
    const check = async () => {
      if (!active || checking || document.visibilityState === 'hidden') return;
      checking = true;
      try {
        const { data } = await api.get('/api/sync/state');
        if (active) acceptServerState(data);
      } catch { /* 旧服务端和临时断网时保留页面，等待下次校验。 */ }
      finally { checking = false; }
    };
    void check(); const timer = setInterval(() => void check(), 60000);
    window.addEventListener('focus', check); document.addEventListener('visibilitychange', check);
    return () => { active = false; clearInterval(timer); window.removeEventListener('focus', check); document.removeEventListener('visibilitychange', check); };
  }, [token, activeUrl]);
}

export function useServerRefresh(refresh: (notice: ServerNotice) => Promise<void>) {
  const run = useEffectEvent(refresh);
  useEffect(() => {
    const handler = (event: Event) => { void run((event as CustomEvent<ServerNotice>).detail).catch(() => {}); };
    window.addEventListener('ting-server-changed', handler);
    return () => window.removeEventListener('ting-server-changed', handler);
  }, []);
}
