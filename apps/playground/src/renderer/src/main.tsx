import React from 'react';
import { createRoot } from 'react-dom/client';
import './styles.css';
import { App } from './App.js';

// Timeline chat UI uses dark-mode hooks (html.dark).
document.documentElement.classList.add('dark');

const container = document.getElementById('root');
if (!container) {
  throw new Error('Root container not found');
}

createRoot(container).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
