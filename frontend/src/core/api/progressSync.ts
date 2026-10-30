import api from './client';
import { useAuthStore } from '../stores/authStore';
import { usePlayerStore } from '../stores/playerStore';
import { ProgressConflictError, ProgressCoordinator } from './progressCoordinator';
import type { ProgressSnapshot } from './progressCoordinator';

const deviceId = globalThis.crypto?.randomUUID?.() ?? `web-${Date.now()}-${Math.random().toString(36).slice(2)}`;
export const progressCoordinator = new ProgressCoordinator({
  async read(book) {
    try { return (await api.get<ProgressSnapshot>(`/api/progress/${encodeURIComponent(book)}`)).data; }
    catch (error) {
      if ((error as { response?: { status?: number } }).response?.status === 404) return { book_id: book, position: 0 };
      throw error;
    }
  },
  async write(value, revision) {
    try { return (await api.post<ProgressSnapshot>('/api/progress', { ...value, device_id: deviceId,
      ...(revision != null ? { base_revision: revision } : {}) })).data; }
    catch (error) {
      const response = (error as { response?: { status?: number; data?: { current?: ProgressSnapshot } } }).response;
      if (response?.status === 409 && response.data?.current) throw new ProgressConflictError(response.data.current);
      throw error;
    }
  },
}, () => {
  const state = useAuthStore.getState(); return `${state.activeUrl}|${state.user?.id}`;
}, conflict => {
  usePlayerStore.getState().setIsPlaying(false);
  window.dispatchEvent(new CustomEvent('ting-progress-conflict', { detail: conflict }));
});
