// Start-up page shown only when the local server could not start.
// Uses the Tauri internals bridge (withGlobalTauri is off).
;(function () {
  var invoke = window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke
  var msg = document.getElementById('message')
  var actions = document.getElementById('actions')
  var port = document.getElementById('port')
  var retry = document.getElementById('retry')
  var title = document.querySelector('h1')
  var feed = document.getElementById('feed')

  // The market data feed's own problem (a taken port), shown with its fix.
  function showFeed(ws) {
    if (!feed) return
    var problem = ws && (ws.state === 'port_in_use' || ws.state === 'failed') && ws.message
    feed.textContent = problem ? ws.message : ''
    feed.hidden = !problem
  }

  function show(status) {
    if (!status) return
    // retry_server answers with the app's own state only; keep the last one.
    if (status.ws) showFeed(status.ws)
    if (status.state === 'running') {
      msg.textContent = 'Opening OpenAlgo.'
      window.location.href = 'http://127.0.0.1:' + status.port + '/'
      return
    }
    if (status.state === 'port_in_use') {
      title.textContent = 'OpenAlgo could not start'
      msg.textContent = status.message
      port.value = status.port
      actions.hidden = false
      return
    }
    if (status.state === 'failed') {
      title.textContent = 'OpenAlgo could not start'
      msg.textContent = status.message
      actions.hidden = false
    }
  }

  if (!invoke) {
    msg.textContent = 'Restart OpenAlgo Desktop.'
    return
  }
  invoke('startup_status').then(show)
  retry.addEventListener('click', function () {
    retry.disabled = true
    msg.textContent = 'Trying again.'
    var p = parseInt(port.value, 10)
    invoke('retry_server', { port: isNaN(p) ? null : p })
      .then(show)
      .catch(function () {
        msg.textContent = 'OpenAlgo still could not start. Choose a different port and try again.'
      })
      .finally(function () {
        retry.disabled = false
      })
  })
})()
