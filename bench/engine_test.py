"""引擎功能测试：冲突三种处理、同盘/跨盘移动、撤销。用 qc.exe 命令行跑，撤销日志写到隔离目录。

用法：python bench/engine_test.py <同盘测试根> <另一个盘上的测试根>
例：python bench/engine_test.py E:/QuickCopy/.tmp/eng C:/Users/me/AppData/Local/Temp/qc-eng
撤销会把复制出来的文件送进回收站。
"""
import os
import shutil
import stat
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
QC = os.path.join(ROOT, "target", "release", "qc.exe")
SAME, OTHER = sys.argv[1], sys.argv[2]
ENV = dict(os.environ, LOCALAPPDATA=os.path.join(SAME, "appdata"))
fails = []


def qc(*args):
    r = subprocess.run([QC, *args], env=ENV, capture_output=True, text=True, encoding="utf-8")
    return r.stdout.strip() + (" | " + r.stderr.strip() if r.stderr.strip() else "")


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def tree(d):
    out = {}
    for base, dirs, files in os.walk(d):
        for n in dirs:
            out[os.path.relpath(os.path.join(base, n), d)] = "<dir>"
        for n in files:
            p = os.path.join(base, n)
            out[os.path.relpath(p, d)] = read(p)
    return out


def fresh(d):
    if os.path.exists(d):
        def unlock(func, p, _):
            os.chmod(p, stat.S_IWRITE)
            func(p)
        shutil.rmtree(d, onerror=unlock)
    os.makedirs(d)
    return d


def check(name, cond, detail=""):
    print(("pass " if cond else "FAIL ") + name + ("" if cond else f"  {detail}"))
    if not cond:
        fails.append(name)


def backups(d):
    return [k for k in tree(d) if ".~qc-" in k]


def case_copy_conflicts():
    base = fresh(os.path.join(SAME, "c1"))
    src, dst = os.path.join(base, "src"), os.path.join(base, "dst")
    write(f"{src}/a.txt", "new")
    write(f"{dst}/a.txt", "old")
    before = tree(dst)
    out = qc("--record", f"{src}/a.txt", dst)
    check("复制冲突默认跳过、不覆盖", tree(dst) == before, out)

    out = qc("--on-conflict", "replace", "--record", f"{src}/a.txt", dst)
    t = tree(dst)
    check("替换：新内容到位", t.get("a.txt") == "new", out)
    bk = backups(dst)
    check("替换：旧文件留成隐藏备份", len(bk) == 1 and t[bk[0]] == "old", t)
    if bk:
        attrs = os.stat(os.path.join(dst, bk[0])).st_file_attributes
        check("替换：备份是隐藏的", attrs & stat.FILE_ATTRIBUTE_HIDDEN != 0)
    out = qc("--undo")
    check("撤销替换：旧文件回来、备份消失", tree(dst) == before, f"{out} {tree(dst)}")

    out = qc("--on-conflict", "keep", "--record", f"{src}/a.txt", dst)
    t = tree(dst)
    check("都保留：起名 a (2).txt", t.get("a (2).txt") == "new" and t.get("a.txt") == "old", t)
    out = qc("--undo")
    check("撤销都保留：只剩原文件", tree(dst) == before, f"{out} {tree(dst)}")


def case_copy_folder_merge():
    base = fresh(os.path.join(SAME, "c2"))
    src, dst = os.path.join(base, "src"), os.path.join(base, "dst")
    write(f"{src}/d/x.txt", "x-new")
    write(f"{src}/d/sub/y.txt", "y")
    os.makedirs(f"{src}/d/empty")
    write(f"{src}/new/z.txt", "z")
    write(f"{dst}/d/x.txt", "x-old")
    write(f"{dst}/d/keep.txt", "keep")
    before = tree(dst)
    out = qc("--on-conflict", "replace", "--record", f"{src}/d", f"{src}/new", dst)
    t = tree(dst)
    ok = t.get("d/x.txt".replace("/", os.sep)) == "x-new" and t.get(os.path.join("d", "sub", "y.txt")) == "y" \
        and t.get(os.path.join("d", "empty")) == "<dir>" and t.get(os.path.join("new", "z.txt")) == "z" \
        and t.get(os.path.join("d", "keep.txt")) == "keep"
    check("合并文件夹并替换冲突", ok, f"{out} {t}")
    out = qc("--undo")
    t = tree(dst)
    check("撤销合并：恢复原样（新建的子文件夹也删掉）", t == before, f"{out} {t}")


def case_move(name, src_root, dst_root):
    base_s = fresh(os.path.join(src_root, name))
    base_d = fresh(os.path.join(dst_root, name))
    src, dst = os.path.join(base_s, "src"), os.path.join(base_d, "dst")
    write(f"{src}/folder/a.txt", "a")
    write(f"{src}/folder/deep/b.txt", "b")
    os.makedirs(f"{src}/folder/empty")
    write(f"{src}/single.txt", "s")
    write(f"{src}/ro.txt", "ro")
    os.chmod(f"{src}/ro.txt", stat.S_IREAD)
    write(f"{src}/merge/m.txt", "m-new")
    write(f"{src}/merge/n.txt", "n")
    write(f"{dst}/merge/m.txt", "m-old")
    write(f"{dst}/merge/o.txt", "o")
    src_before, dst_before = tree(src), tree(dst)
    items = [f"{src}/folder", f"{src}/single.txt", f"{src}/ro.txt", f"{src}/merge"]
    out = qc("--move", "--on-conflict", "replace", "--record", *items, dst)
    t = tree(dst)
    expect = {os.path.join("folder", "a.txt"): "a", os.path.join("folder", "deep", "b.txt"): "b",
              os.path.join("folder", "empty"): "<dir>", "single.txt": "s", "ro.txt": "ro",
              os.path.join("merge", "m.txt"): "m-new", os.path.join("merge", "n.txt"): "n",
              os.path.join("merge", "o.txt"): "o"}
    check(f"{name}：目标内容齐全", all(t.get(k) == v for k, v in expect.items()), f"{out} {t}")
    check(f"{name}：源已清空", tree(src) == {}, tree(src))
    out = qc("--undo")
    check(f"{name}：撤销后源恢复", tree(src) == src_before, f"{out} {tree(src)}")
    check(f"{name}：撤销后目标恢复", tree(dst) == dst_before, f"{out} {tree(dst)}")
    return out


def case_move_keep_both(name, src_root, dst_root):
    base_s = fresh(os.path.join(src_root, name))
    base_d = fresh(os.path.join(dst_root, name))
    src, dst = os.path.join(base_s, "src"), os.path.join(base_d, "dst")
    write(f"{src}/k.txt", "new")
    write(f"{dst}/k.txt", "old")
    out = qc("--move", "--on-conflict", "keep", "--record", f"{src}/k.txt", dst)
    t = tree(dst)
    check(f"{name}：移动时都保留", t == {"k.txt": "old", "k (2).txt": "new"} and tree(src) == {}, f"{out} {t}")
    out = qc("--undo")
    check(f"{name}：撤销", tree(dst) == {"k.txt": "old"} and tree(src) == {"k.txt": "new"}, f"{out} {tree(dst)} {tree(src)}")


def case_move_into_self():
    base = fresh(os.path.join(SAME, "c5"))
    write(f"{base}/p/q.txt", "q")
    out = qc("--move", f"{base}/p", f"{base}/p")
    check("移动到自己里面被拒绝", "里面" in out and tree(base) == {"p": "<dir>", os.path.join("p", "q.txt"): "q"}, out)


fresh(SAME)
fresh(OTHER)
case_copy_conflicts()
case_copy_folder_merge()
print("  ", case_move("同盘移动", SAME, SAME))
print("  ", case_move("跨盘移动", OTHER, SAME))
case_move_keep_both("同盘", SAME, SAME)
case_move_keep_both("跨盘", OTHER, SAME)
case_move_into_self()
print(f"\n{len(fails)} 项失败" if fails else "\n全部通过")
sys.exit(1 if fails else 0)
