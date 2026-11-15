import fs from 'node:fs';

const path = 'src/js/versions.js';
let src = fs.readFileSync(path, 'utf8');
const start = src.indexOf('    // 下载源：手动刷新');
const end = src.indexOf('    // 安装进度（全局事件');
if (start < 0 || end < 0 || end < start) {
    console.error('MARK_NOT_FOUND', start, end);
    process.exit(1);
}
src = src.slice(0, start) + src.slice(end);
fs.writeFileSync(path, src);
console.log('stripped_chars=' + (end - start));
