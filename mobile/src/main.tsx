import React from 'react';
import ReactDOM from 'react-dom/client';
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom';
import Pairing from './pages/Pairing';
import Login from './pages/Login';
import Home from './pages/Home';
import Recepcion from './pages/Recepcion';
import Consulta from './pages/Consulta';
import { RequireAuth } from './lib/auth';
import './styles.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <BrowserRouter>
      <Routes>
        <Route path="/pairing" element={<Pairing />} />
        <Route path="/login" element={<Login />} />
        <Route path="/" element={<RequireAuth><Home /></RequireAuth>} />
        <Route path="/recepcion" element={<RequireAuth><Recepcion /></RequireAuth>} />
        <Route path="/consulta" element={<RequireAuth><Consulta /></RequireAuth>} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </BrowserRouter>
  </React.StrictMode>,
);
