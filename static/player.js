let jwtToken = localStorage.getItem('token');

async function login() {
    const username = document.getElementById('username').value;
    const password = document.getElementById('password').value;

    const res = await fetch('/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, password }),
        credentials: 'include' // VERY IMPORTANT: allows cookies to be sent
    });

    if (res.ok) {
        document.getElementById('loginForm').style.display = 'none';
        document.getElementById('channelButtons').style.display = 'block';
        fetchChannels();
    } else {
        alert("Login failed");
    }
}

async function fetchChannels() {
    const res = await fetch('/channels', { credentials: 'include' });

    if (!res.ok) {
        alert("You must log in first.");
        return;
    }

    const channels = await res.json();
    const container = document.getElementById('channelButtons');
    container.innerHTML = '';

    channels.forEach(channel => {
        const btn = document.createElement('button');
        btn.textContent = `Play ${channel.name}`;
        btn.onclick = () => loadAndInitStream(channel.id);
        container.appendChild(btn);
    });
}

async function loadAndInitStream(id) {
    const res = await fetch(`/init/${id}`, { credentials: 'include' });
    const result = await res.text();
    console.log(result);
    setTimeout(() => loadStream(id), 2000);
}

function loadStream(id) {
    const video = document.getElementById('videoPlayer');
    const streamUrl = `/streams/${id}/master.m3u8`;

    if (Hls.isSupported()) {
        const hls = new Hls(); // no xhrSetup needed anymore
        hls.loadSource(streamUrl);
        hls.attachMedia(video);
        hls.on(Hls.Events.MANIFEST_PARSED, function () {
            video.play();
        });
        hls.on(Hls.Events.ERROR, function (event, data) {
            console.error('HLS Error:', data);
        });
    } else if (video.canPlayType('application/vnd.apple.mpegurl')) {
        video.src = streamUrl;
        video.addEventListener('loadedmetadata', function () {
            video.play();
        });
    } else {
        alert('HLS not supported in this browser');
    }
}

// Auto attempt fetch if token exists
if (jwtToken) {
    document.getElementById('loginForm').style.display = 'none';
    document.getElementById('channelButtons').style.display = 'block';
    fetchChannels();
}

