import React from 'react';
import ReactDOM from 'react-dom/client';
import { App } from './App';
import { RenderRecovery } from './RenderRecovery';
import './styles.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode><RenderRecovery><App /></RenderRecovery></React.StrictMode>,
);
