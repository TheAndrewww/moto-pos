import { useEffect, useRef, useState } from 'react';
import { BrowserMultiFormatReader } from '@zxing/browser';

interface Props {
  onDetected: (codigo: string) => void;
  onClose: () => void;
}

export default function Scanner({ onDetected, onClose }: Props) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const reader = new BrowserMultiFormatReader();
    let stopFn: (() => void) | null = null;
    let cancelled = false;

    (async () => {
      try {
        const devices = await BrowserMultiFormatReader.listVideoInputDevices();
        if (cancelled) return;
        if (!devices.length) {
          setError('No se encontró cámara.');
          return;
        }
        const back = devices.find(d => /back|rear|trás|environment/i.test(d.label)) ?? devices[devices.length - 1];
        const controls = await reader.decodeFromVideoDevice(
          back.deviceId,
          videoRef.current!,
          (result) => {
            if (result) {
              onDetected(result.getText());
            }
          },
        );
        stopFn = () => controls.stop();
      } catch (e: any) {
        setError(e.message || 'No se pudo acceder a la cámara.');
      }
    })();

    return () => {
      cancelled = true;
      if (stopFn) stopFn();
    };
  }, [onDetected]);

  return (
    <div style={{
      position: 'fixed', inset: 0, background: '#000', zIndex: 100,
      display: 'flex', flexDirection: 'column',
    }}>
      <div style={{
        padding: 'calc(12px + env(safe-area-inset-top)) 16px 12px',
        display: 'flex', alignItems: 'center', gap: 12,
        color: '#fff',
      }}>
        <button
          onClick={onClose}
          style={{ background: 'none', border: 'none', color: '#fff', fontSize: 20, padding: 4 }}
        >
          ✕
        </button>
        <span style={{ fontSize: 14 }}>Escanea un código</span>
      </div>
      <div style={{ flex: 1, position: 'relative', overflow: 'hidden' }}>
        <video ref={videoRef} style={{ width: '100%', height: '100%', objectFit: 'cover' }} />
        <div style={{
          position: 'absolute', inset: 0,
          display: 'flex', alignItems: 'center', justifyContent: 'center',
          pointerEvents: 'none',
        }}>
          <div style={{
            width: '70%', aspectRatio: '3/2',
            border: '2px solid #fff', borderRadius: 12,
            boxShadow: '0 0 0 9999px rgba(0,0,0,0.4)',
          }} />
        </div>
      </div>
      {error && (
        <div style={{ padding: 16, color: '#fff', textAlign: 'center' }}>{error}</div>
      )}
    </div>
  );
}
