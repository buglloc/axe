const value = (id, text) => { document.getElementById(id).textContent = String(text); };
const cells = (row, fields) => {
  for (const field of fields) {
    const cell = document.createElement('td');
    cell.textContent = String(field);
    row.append(cell);
  }
};
async function refresh() {
  const error = document.getElementById('error');
  try {
    const response = await fetch('/api/v1/status', { cache: 'no-store' });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const status = await response.json();
    value('count', status.clients.length);
    value('uptime', `${status.uptime_seconds}s`);
    value('tcp', status.tcp_control);
    value('quic', status.quic_control);
    const list = document.getElementById('clients');
    list.replaceChildren();
    for (const client of status.clients) {
      const row = document.createElement('tr');
      cells(row, [client.client_id, client.transport, client.peer, client.public_address, `${client.connected_seconds}s`]);
      list.append(row);
    }
    if (!status.clients.length) {
      const row = document.createElement('tr');
      const cell = document.createElement('td');
      cell.colSpan = 5;
      cell.textContent = 'No connected clients';
      row.append(cell);
      list.append(row);
    }
    error.hidden = true;
    value('updated', `Updated ${new Date().toLocaleTimeString()}`);
  } catch (cause) {
    error.textContent = `Unable to load relay status: ${cause.message}`;
    error.hidden = false;
    value('updated', 'Disconnected');
  }
}
refresh();
setInterval(refresh, 5000);
