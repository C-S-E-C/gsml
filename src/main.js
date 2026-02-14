const app = {
    channel: 'ALPHA',
    version: '1.0.0',

}
const invoke = window.__TAURI_INTERNALS__.invoke
const thesquidsays = [
    "Ayo, stop touching me!",
    "I'm not a toy!",
    "Leave me alone!",
    "Don't you understand?",
    "Hell nah!"
]
console.log(`[ALPHA] ${app.version}`);
console.log(`[PATH] ${location.pathname}`)
// ====================================================================================================================
async function load() {
    await localStorage.setItem('JAVA', JSON.stringify(await invoke("list_directories",{path:"C:/Program Files/Java"})));
    console.log("INITIALIZED SUCCESFULLY");
}

function sidebar_button(name,btn) {
    document.querySelectorAll('.sidebar button').forEach(b => b.classList.remove('active'));
    btn.classList.add('active');
    document.getElementById("main").src=`components/${name}.html`
}

function notice(msg) {
    
}

function requiredjava (date) {
    // date is YYY-MM-DD
    date = new Date(date);
    if (date < new Date('2021-05-12')) { // May 12, 2021
        return 8;
    } else if (date < new Date('2021-09-01')) { // September 1, 2021
        return 16
    }
}

var logoclickedtimes = 0;
function logoclick() {
    if (logoclickedtimes == thesquidsays.length) {
        document.getElementById("logo").src = "assets/him.png";
        document.getElementById("js-css").innerHTML += `:root {
           --root-bgcolor: #320000;
           --text-color: #f00;
           --bg-color: #4d0000;
        }`
        alert("YOU PISSED ME OFF!");
        logoclickedtimes = 0;
    } else {
        alert("The squid:\n"+thesquidsays[logoclickedtimes]);
    }
    logoclickedtimes++;
}
//after load
document.addEventListener('DOMContentLoaded', async () => {
    if (location.pathname == '/') {
        if (localStorage.getItem("StartTimes")) {
            if (!localStorage.getItem("MC_PATH")) {
                await localStorage.setItem('MC_PATH', (await invoke('get_env_var', {name: 'APPDATA'})).replace(/\\/g, "/") + "/.minecraft");
            }
            document.getElementsByClassName("sidebar")[0].style.display = "flex"
            localStorage.setItem("StartTimes",Number(localStorage.getItem("StartTimes"))+1)
        } else {
            location.pathname = '/user_license.html'
        }
    }
    if (location.pathname == '/components/versions.html') {
        const list = document.getElementById('version-list');
        const versions = JSON.parse(localStorage.getItem('MC_VERSIONS') || '[]');
        
        // 安全的 HTML 生成
        const createVersionHTML = (version) => {
            const lowerVersion = version.toLowerCase();
            let imgsrc = "../assets/";
            
            if (lowerVersion.includes("fabric")) {
                imgsrc += "fabric.png";
            } else if (lowerVersion.includes("neoforge")) {
                imgsrc += "neoforge.png";
            } else if (lowerVersion.includes("forge")) {
                imgsrc += "anvil.png";
            } else if (version.includes("1.")) {
                imgsrc += "grass_block.png";
            } else if (lowerVersion.includes("w") || lowerVersion.includes("snapshot")) {
                imgsrc += "dirt.png";
            } else if (lowerVersion.startsWith("rd") || lowerVersion.startsWith("a") || lowerVersion.startsWith("b") || lowerVersion.startsWith("c") || lowerVersion.startsWith("inf")) {
                imgsrc += "cobblestone.png";
            } else {
                imgsrc += "music_disc_11.png";
            }
            
            // 使用模板字符串，但确保内容安全
            return `<li class="version">
                <img src="${imgsrc}" alt="${version}" />
                <h3>${version.replace(/[&<>"']/g, c => ({
                    '&': '&amp;',
                    '<': '&lt;',
                    '>': '&gt;',
                    '"': '&quot;',
                    "'": '&#39;'
                }[c]))}</h3>
            </li>`;
        };
        
        // 一次性设置 innerHTML 比多次追加更高效
        list.innerHTML = versions.map(createVersionHTML).join('');
    }
})

