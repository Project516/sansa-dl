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
const status = $('#status');
const save = $('#save');
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
    status.textContent = 'Enter your proxy URL before downloading.';
    proxy.focus();
    return;
  }
  $('#submit').disabled = true;
  save.hidden = true;
  if (downloadURL) URL.revokeObjectURL(downloadURL);
  let ytdlp;
  let ffmpeg;
  try {
    status.textContent = 'Starting downloader. This can take a moment…';
    ytdlp = await createYtDlp({ corsProxy: proxy.value.trim(), corsProxyKey: key.value || undefined, wheelURL });
    status.textContent = 'Downloading audio…';
    const info = await ytdlp.extractInfo($('#url').value, { noplaylist: true });
    const files = await ytdlp.download(info, { format: 'bestaudio/best', noplaylist: true }, {
      onProgress: ({ downloadedBytes, totalBytes }) => {
        if (downloadedBytes && totalBytes) status.textContent = `Downloading audio… ${Math.min(100, Math.round(downloadedBytes / totalBytes * 100))}%`;
      },
    });
    if (files.length !== 1) throw new Error('The link did not produce one audio file.');
    status.textContent = 'Preparing MP3 for your Sansa…';
    ffmpeg = new FFmpeg();
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
    status.textContent = 'Your MP3 is ready.';
  } catch (error) {
    status.textContent = `Could not prepare the song: ${error.message}`;
  } finally {
    ytdlp?.terminate();
    ffmpeg?.terminate();
    $('#submit').disabled = false;
  }
});
