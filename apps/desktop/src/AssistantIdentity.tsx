import { createContext, useContext, useEffect, useState, type ReactNode } from 'react';
import { call, desktop } from './bridge';

const DEFAULT_NAME = '小舵同学';
const Identity = createContext({ name: DEFAULT_NAME, saved: (_name: string) => {} });

/** Use saved global settings everywhere; editing an unsaved wake word changes no identity. */
export function AssistantIdentityProvider({ children }: { children: ReactNode }) {
  const [name, setName] = useState(DEFAULT_NAME);
  const [error, setError] = useState('');
  useEffect(() => {
    if (!desktop) return;
    let live = true;
    void call<{ wake_word: string }>({ kind: 'assistant_settings' }).then(value => {
      if (live) setName(value.wake_word || DEFAULT_NAME);
    }).catch(() => { if (live) setError('助手名称设置读取失败，请重新打开应用。'); });
    return () => { live = false; };
  }, []);
  return <Identity.Provider value={{ name, saved: value => setName(value || DEFAULT_NAME) }}>
    {error && <div role="alert" className="banner error">{error}</div>}
    {children}
  </Identity.Provider>;
}
export const useAssistantIdentity = () => useContext(Identity);
