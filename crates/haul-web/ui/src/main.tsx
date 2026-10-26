import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './index.css';
import { App } from '@/app';
import { openAdd } from '@/lib/add';
import { followRoute } from '@/lib/route';
import { connect } from '@/lib/store';

const root = document.getElementById('app');
if (!root) throw new Error('Missing #app');

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

const follow = () => {
  const links = followRoute();
  if (links !== null) openAdd(links);
};
follow();
addEventListener('hashchange', follow);
connect();
