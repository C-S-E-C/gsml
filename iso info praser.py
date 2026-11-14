# iso info parser
import sys
import requests

mirror_links = [
    'https://mirrors.njupt.edu.cn/isoinfo.json',
    'https://mirrors.tuna.tsinghua.edu.cn/static/status/isoinfo.json',
    'https://mirrors.ha.edu.cn/static/isoinfo.json',
    'https://mirrors.bfsu.edu.cn/static/status/isoinfo.json',
    'https://mirror.nyist.edu.cn/static/isoinfo.json',
    'https://mirrors-metadata.cqulug.club/isoinfo.json',
]


def search_all_for(endpoints, distro_name, *, strict=False, timeout=15):
    """
    在所有镜像的 isoinfo.json 中搜索指定发行版。

    参数:
        endpoints:   isoinfo.json 地址列表
        distro_name: 要搜索的发行版名称（大小写不敏感）
        strict:      True  -> 发行版名必须完全相等
                     False -> 发行版名包含搜索词即可
        timeout:     单个请求超时时间（秒）

    返回:
        list[dict]: 去重后的条目，每个 dict 形如
                    {'name': ..., 'url': ..., 'size': ..., 'mtime': ...}
    """
    distro_name = distro_name.upper()
    seen = set()
    results = []

    session = requests.Session()
    session.headers['User-Agent'] = 'isoinfo-parser/1.0'

    for link in endpoints:
        try:
            r = session.get(link, timeout=timeout)
            r.raise_for_status()
            distros = r.json()
        except (requests.RequestException, ValueError) as e:
            print(f"skip {link}: {e}", file=sys.stderr)
            continue

        for distro in distros:
            name = distro.get('distro', '').upper()
            matched = (name == distro_name) if strict else (distro_name in name)
            if not matched:
                continue

            for entry in distro.get('urls') or []:
                # 部分镜像给的是纯字符串，大部分给的是 dict
                if isinstance(entry, str):
                    entry = {
                        'url': entry,
                        'name': entry.rsplit('/', 1)[-1],
                    }
                if not isinstance(entry, dict):
                    continue

                url = entry.get('url')
                if url and url not in seen:
                    seen.add(url)
                    results.append(entry)

    return results


def main():
    results = search_all_for(mirror_links, 'Adoptium')

    if not results:
        print("未找到匹配的发行版。", file=sys.stderr)
        return

    print(f"共找到 {len(results)} 个文件：\n")
    print(" "*15,"\t"," ")
    print("Name","\t","Url")
    for item in results:
        name = item.get('name') or item['url'].rsplit('/', 1)[-1]
        print(name,"\t",item['url'])


if __name__ == '__main__':
    main()