import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './index.css';
import { App } from '@/app';
import { openAdd } from '@/lib/add';
import { takeAddRoute } from '@/lib/route';
import { connect } from '@/lib/store';

const root = document.getElementById('app');
if (!root) throw new Error('Missing #app');

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

const links = takeAddRoute();
if (links !== null) openAdd(links);
addEventListener('hashchange', () => {
  const more = takeAddRoute();
  if (more !== null) openAdd(more);
});
connect();
