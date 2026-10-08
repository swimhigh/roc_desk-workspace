import { create } from "zustand";
import { aiProviderService, type AiProvider, type AiProviderInput } from "../services";
import { formatError } from "../utils/error";

/** AI provider CRUD only -- trimmed from the host's `aiChatStore`, which
 * also manages a general-purpose streaming chat panel this tool doesn't
 * have. The coding agent needs at least one configured provider to start a
 * session; this store backs the provider picker/management dialog. */
interface AiProviderState {
  providers: AiProvider[];
  modelsByProvider: Record<string, string[]>;
  error: string | null;

  loadProviders: () => Promise<void>;
  createProvider: (input: AiProviderInput) => Promise<void>;
  updateProvider: (id: string, input: AiProviderInput) => Promise<void>;
  deleteProvider: (id: string) => Promise<void>;
  /** Fetches a provider's model list; if its currently configured default
   * model isn't in the list (or was never set), auto-picks the first one
   * and persists it. Failure (no `/models` support, network issue) is
   * silent -- falls back to whatever default model is already configured,
   * this is a nice-to-have, not a critical path. */
  fetchModels: (providerId: string) => Promise<void>;
}

export const useAiProviderStore = create<AiProviderState>((set, get) => ({
  providers: [],
  modelsByProvider: {},
  error: null,

  loadProviders: async () => {
    try {
      set({ providers: await aiProviderService.list() });
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  createProvider: async (input) => {
    const created = await aiProviderService.create(input);
    set((s) => ({ providers: [...s.providers, created] }));
  },

  updateProvider: async (id, input) => {
    const updated = await aiProviderService.update(id, input);
    set((s) => ({ providers: s.providers.map((p) => (p.id === id ? updated : p)) }));
  },

  deleteProvider: async (id) => {
    await aiProviderService.delete(id);
    set((s) => ({ providers: s.providers.filter((p) => p.id !== id) }));
  },

  fetchModels: async (providerId) => {
    try {
      const models = await aiProviderService.listModels(providerId);
      set((s) => ({ modelsByProvider: { ...s.modelsByProvider, [providerId]: models } }));
      const provider = get().providers.find((p) => p.id === providerId);
      if (provider && models.length > 0 && !models.includes(provider.model)) {
        await get().updateProvider(providerId, {
          name: provider.name,
          api_base: provider.api_base,
          api_key: null,
          model: models[0],
          is_local: provider.is_local,
          wire_api: provider.wire_api,
          reasoning_effort: provider.reasoning_effort,
          context_window_tokens: provider.context_window_tokens,
        });
      }
    } catch {
      // Silent failure, see the field doc above.
    }
  },
}));
