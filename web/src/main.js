import './style.css';
import { createYtDlp } from '@project516/yt-dlp-wasm';
import { FFmpeg } from '@ffmpeg/ffmpeg';
import coreURL from '../node_modules/@ffmpeg/core/dist/esm/ffmpeg-core.js?url';
import wasmURL from '../node_modules/@ffmpeg/core/dist/esm/ffmpeg-core.wasm?url';
import { safeName, validateMp3 } from './output.js';

// The wheel name changes with each yt-dlp release.
const [wheelURL] = Object.values(import.meta.glob('../node_modules/@project516/yt-dlp-wasm/dist/*.whl', { query: '?url', import: 'default', eager: true }));
const $ = (selector) => document.querySelector(selector);
const proxy = $('#proxy');
const key = $('#key');
const save = $('#save');
const meter = $('#meter');

// fraction: undefined hides the bar, null shows it busy, 0 to 1 fills it.
function screen(title, detail = '', fraction) {
  $('#screen-title').textContent = title;
  $('#screen-detail').textContent = detail;
  meter.hidden = fraction === undefined;
  meter.classList.toggle('busy', fraction === null);
  const value = Math.round(Math.min(1, Math.max(0, fraction ?? 0)) * 100);
  $('#meter-fill').style.width = fraction === null ? '' : `${value}%`;
  if (fraction === null) meter.removeAttribute('aria-valuenow');
  else meter.setAttribute('aria-valuenow', value);
}

let downloadURL;
try {
  proxy.value = localStorage.getItem('proxy') || '';
  key.value = localStorage.getItem('proxyKey') || '';
} catch { /* Storage can be disabled. */ }

$('#proxy-form').addEventListener('submit', (event) => {
  event.preventDefault();
  try {
    localStorage.setItem('proxy', proxy.value.trim());
    localStorage.setItem('proxyKey', key.value);
    $('#settings-status').textContent = 'Settings saved.';
  } catch {
    $('#settings-status').textContent = 'Settings could not be saved. They will work until this page closes.';
  }
});

$('#download-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (!proxy.value.trim()) {
    document.querySelector('details').open = true;
    screen('Set up a proxy', 'Add your proxy URL in Proxy settings below.');
    proxy.focus();
    return;
  }
  $('#submit').disabled = true;
  save.hidden = true;
  if (downloadURL) URL.revokeObjectURL(downloadURL);
  let ytdlp;
  let ffmpeg;
  try {
    screen('Starting', 'Loading the downloader. The first time takes a moment.', null);
    ytdlp = await createYtDlp({ corsProxy: proxy.value.trim(), corsProxyKey: key.value || undefined, wheelURL });
    screen('Finding', 'Reading the link', null);
    const info = await ytdlp.extractInfo($('#url').value, { noplaylist: true });
    const name = info.title || 'Your song';
    screen('Downloading', name, null);
    const files = await ytdlp.download(info, { format: 'bestaudio/best', noplaylist: true }, {
      onProgress: ({ downloadedBytes, totalBytes }) => {
        if (downloadedBytes && totalBytes) screen('Downloading', name, downloadedBytes / totalBytes);
      },
    });
    if (files.length !== 1) throw new Error('The link did not produce one audio file.');
    screen('Converting', name, null);
    ffmpeg = new FFmpeg();
    ffmpeg.on('progress', ({ progress }) => {
      if (progress >= 0 && progress <= 1) screen('Converting', name, progress);
    });
    await ffmpeg.load({ coreURL, wasmURL });
    await ffmpeg.writeFile('input', files[0].data);
    const metadata = [];
    for (const [field, value] of Object.entries({ title: info.title, artist: info.artist || info.uploader, album: info.album || info.artist || info.uploader })) {
      if (typeof value === 'string' && value.trim()) metadata.push('-metadata', `${field}=${Array.from(value).slice(0, 512).join('')}`);
    }
    const result = await ffmpeg.exec(['-i', 'input', '-map', '0:a:0', '-vn', '-map_metadata', '-1', ...metadata, '-c:a', 'libmp3lame', '-q:a', '0', '-ar', '44100', '-ac', '2', '-id3v2_version', '3', '-write_id3v1', '0', 'output.mp3']);
    if (result !== 0) throw new Error('The audio could not be converted to MP3.');
    const bytes = await ffmpeg.readFile('output.mp3');
    validateMp3(bytes);
    downloadURL = URL.createObjectURL(new Blob([bytes], { type: 'audio/mpeg' }));
    save.href = downloadURL;
    save.download = safeName(files[0].name || info.title || 'audio');
    save.hidden = false;
    screen('Ready to save', name, 1);
  } catch (error) {
    screen('Stopped', `Could not make the MP3: ${error?.message ?? error}`);
  } finally {
    ytdlp?.terminate();
    ffmpeg?.terminate();
    $('#submit').disabled = false;
  }
});
