# Keeps a program's windows behind every other window, without activating them, for as long as it
# runs: a check pictures a build while the person works, and a build's window on top of their work
# catches a stray click. Started before the program, it says "ready", reads the program's process
# id from stdin, and sends each window of that process and its descendants to the bottom of the
# z-order as soon as it is shown. Windows still draws a covered window, and a picture of it is taken
# with PrintWindow, which doesn't depend on what covers it. Ends when the program or the process
# that started the helper ends, or when stdin closes before an id arrives.
$ErrorActionPreference = 'Stop'
$owner = Get-Process -Id OWNER_PID; $null = $owner.Handle
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Threading;

public static class CraftAnythingBehind {
    delegate bool EnumProc(IntPtr hwnd, IntPtr param);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr param);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern int GetWindowLong(IntPtr hwnd, int index);
    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("kernel32.dll")] static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern bool Process32FirstW(IntPtr snap, ref Entry e);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern bool Process32NextW(IntPtr snap, ref Entry e);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct Entry {
        public uint size, usage, pid;
        public IntPtr heap;
        public uint module, threads, parent;
        public int priority;
        public uint flags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string exe;
    }

    static readonly IntPtr Top = IntPtr.Zero;
    static readonly IntPtr Bottom = new IntPtr(1);
    // SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOOWNERZORDER
    const uint Flags = 0x0001 | 0x0002 | 0x0010 | 0x0200;
    // SWP_ASYNCWINDOWPOS: ask, and don't wait for the window's program to answer.
    const uint Async = 0x4000;

    // Processes that ran before the program started (taken while warming up, before it starts),
    // so never its descendants: Windows reuses ids, and a recorded parent id can name a process
    // that's not that parent's, so an older program could otherwise join the tree.
    static HashSet<uint> before = new HashSet<uint>();

    // All process ids, with their recorded parents.
    static List<KeyValuePair<uint, uint>> Processes() {
        var parents = new List<KeyValuePair<uint, uint>>();
        IntPtr snap = CreateToolhelp32Snapshot(2, 0);
        if (snap == new IntPtr(-1)) return parents;
        try {
            var e = new Entry { size = (uint)Marshal.SizeOf(typeof(Entry)) };
            for (bool ok = Process32FirstW(snap, ref e); ok; ok = Process32NextW(snap, ref e)) {
                parents.Add(new KeyValuePair<uint, uint>(e.pid, e.parent));
            }
        } finally { CloseHandle(snap); }
        return parents;
    }

    // The root and every process started under it since the program began.
    static HashSet<uint> Tree(uint root) {
        var tree = new HashSet<uint> { root };
        var parents = Processes();
        for (bool grew = true; grew; ) {
            grew = false;
            foreach (var p in parents) {
                if (p.Key != p.Value && !before.Contains(p.Key) && !tree.Contains(p.Key) && tree.Contains(p.Value)) { tree.Add(p.Key); grew = true; }
            }
        }
        return tree;
    }

    // The program's id from stdin, or null once the owner ends or stdin closes first. The read
    // is on a thread of its own: the console's reader blocks even when asked to read async.
    static string WaitForId(Process owner) {
        string line = null;
        var read = new Thread(() => { line = Console.In.ReadLine(); });
        read.IsBackground = true;
        read.Start();
        while (!read.Join(100)) {
            if (owner.HasExited) return null;
        }
        return line;
    }

    // Each shown window of the tree that has another program's ordinary window (shown, not
    // always-on-top) beneath it goes to the bottom. Every time, not once: a program can raise its
    // window again as it shows it, and one already at the bottom has nothing beneath it. Moving
    // another program's window waits for that program to answer, and a starting program is busy
    // (a graphics device can take half a second) while its window already shows, so the move is
    // only queued (asked again every 200 ms, not every pass) and the person's own window, whose
    // program answers, is raised above it at once.
    static void Lower(HashSet<uint> tree, Dictionary<IntPtr, int> asked) {
        var order = new List<KeyValuePair<IntPtr, bool>>();
        EnumWindows((hwnd, _) => {
            if (IsWindowVisible(hwnd)) {
                uint pid;
                GetWindowThreadProcessId(hwnd, out pid);
                order.Add(new KeyValuePair<IntPtr, bool>(hwnd, tree.Contains(pid)));
            }
            return true;
        }, IntPtr.Zero);
        IntPtr front = GetForegroundWindow();
        int frontAt = order.FindIndex(w => w.Key == front);
        bool raiseFront = false;
        // Top to bottom, so walk up from the bottom.
        bool otherBelow = false;
        for (int i = order.Count - 1; i >= 0; i--) {
            var w = order[i];
            if (!w.Value) {
                if (!Topmost(w.Key)) otherBelow = true;
            } else if (otherBelow) {
                if (frontAt > i && !order[frontAt].Value && !Topmost(front)) raiseFront = true;
                int last;
                if (!asked.TryGetValue(w.Key, out last) || Environment.TickCount - last > 200) {
                    SetWindowPos(w.Key, Bottom, 0, 0, 0, 0, Flags | Async);
                    asked[w.Key] = Environment.TickCount;
                }
            }
        }
        if (raiseFront) SetWindowPos(front, Top, 0, 0, 0, 0, Flags | Async);
    }

    static bool Topmost(IntPtr hwnd) {
        // WS_EX_TOPMOST
        return (GetWindowLong(hwnd, -20) & 0x8) != 0;
    }

    // Load and compile everything Run calls, so the first window isn't missed while it does.
    public static void WarmUp(Process owner) {
        foreach (var p in Processes()) before.Add(p.Key);
        Tree((uint)owner.Id);
        Lower(new HashSet<uint>(), new Dictionary<IntPtr, int>());
        Process.GetProcessById(owner.Id).Dispose();
    }

    // Wait for the program's id, then keep its windows behind until it or the owner ends. All in
    // compiled code: a PowerShell step here (Get-Process takes about half a second the first
    // time) left the first window on top while it ran.
    public static void Serve(Process owner) {
        string line = WaitForId(owner);
        int id;
        if (line == null || !int.TryParse(line.Trim(), out id)) return;
        Process program;
        try { program = Process.GetProcessById(id); } catch (ArgumentException) { return; }
        Run(program, owner);
    }

    static void Run(Process program, Process owner) {
        uint root = (uint)program.Id;
        var tree = Tree(root);
        var asked = new Dictionary<IntPtr, int>();
        for (int n = 0; !program.HasExited && !owner.HasExited; n++) {
            if (n % 10 == 0) tree = Tree(root);
            Lower(tree, asked);
            Thread.Sleep(10);
        }
    }
}
'@
[CraftAnythingBehind]::WarmUp($owner)
[Console]::Out.WriteLine('ready')
[Console]::Out.Flush()
[CraftAnythingBehind]::Serve($owner)
