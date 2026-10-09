// Bridges native-only actions to the shared web UI. Absent when the UI is opened in a phone browser.
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('lanclipNative', {
  copy: (id) => ipcRenderer.invoke('item:copy', id),
  open: (id) => ipcRenderer.invoke('item:open', id),
  show: (id) => ipcRenderer.invoke('item:show', id),
  saveAs: (id) => ipcRenderer.invoke('item:saveAs', id),
  startDrag: (id) => ipcRenderer.send('item:drag', id),
  sendClipboard: () => ipcRenderer.invoke('clipboard:send'),
});
