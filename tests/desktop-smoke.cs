using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Windows.Forms;

// Run via desktop-smoke.ps1. The harness never switches the input desktop or
// synthesizes user input. Window messages target only its isolated processes.
internal static class DesktopSmoke {
    const uint WmClose = 0x10, RustileShortcut = 0x8002;
    const uint TrayCallback = 0x8000 + 20;
    const int TraySelect = 0x400, TrayContext = 0x7b, TrayKeyboard = 0x401;
    const int ShowFixtureDialog = 0x8001, RequestFocus = 0x8002;
    const int ExtendedFrameBounds = 9, WindowCornerPreference = 33;
    static readonly int[] FixtureCorners = { 2, 3, 1 }; // round, small, square

    [StructLayout(LayoutKind.Sequential)]
    struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)]
    struct GuiThreadInfo {
        public int size, flags;
        public IntPtr active, focus, capture, menuOwner, moveSize, caret;
        public Rect caretBounds;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct StartupInfo {
        public int cb;
        public string reserved, desktop, title;
        public int x, y, width, height, columns, lines, fill, flags;
        public short show, reservedSize;
        public IntPtr reservedBytes, input, output, error;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct ProcessInfo {
        public IntPtr process, thread;
        public int pid, tid;
    }
    delegate bool EnumWindow(IntPtr window, IntPtr arg);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateDesktop(string name, IntPtr device, IntPtr mode, uint flags, uint access, IntPtr security);
    [DllImport("user32.dll", SetLastError = true)] static extern bool SetThreadDesktop(IntPtr desktop);
    [DllImport("user32.dll")] static extern IntPtr GetThreadDesktop(uint id);
    [DllImport("user32.dll")] static extern bool CloseDesktop(IntPtr desktop);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll")] static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool TerminateProcess(IntPtr handle, uint code);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetExitCodeProcess(IntPtr handle, out uint code);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool CreateProcess(string file, StringBuilder args, IntPtr processSecurity, IntPtr threadSecurity,
        bool inherit, uint flags, IntPtr env, string workingDir, ref StartupInfo startup, out ProcessInfo process);
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindow visit, IntPtr arg);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumWindow visit, IntPtr arg);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out int pid);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr hwnd, uint cmd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder name, int size);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool IsWindowEnabled(IntPtr hwnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint msg, UIntPtr wp, StringBuilder text, uint flags,
        uint timeout, out UIntPtr result);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr GetProp(IntPtr hwnd, string name);
    [DllImport("user32.dll")] static extern uint GetDpiForSystem();
    [DllImport("dwmapi.dll")] static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attribute, out int value, int size);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attribute, out Rect value, int size);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", SetLastError = true)] static extern bool GetGUIThreadInfo(int tid, ref GuiThreadInfo info);
    [DllImport("user32.dll")] static extern void NotifyWinEvent(uint winEvent, IntPtr hwnd, int objectId, int childId);
    [DllImport("user32.dll", SetLastError = true)] static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll", SetLastError = true)] static extern bool PostThreadMessage(int tid, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll", SetLastError = true)] static extern bool RegisterHotKey(IntPtr hwnd, int id, uint modifiers, uint vk);
    [DllImport("user32.dll")] static extern bool UnregisterHotKey(IntPtr hwnd, int id);

    // Fixture six reproduces an application whose declared minimum outer
    // height exceeds half of the two-row test area.
    static int TallMinimum(Rectangle work) {
        int height = Math.Min(work.Height, Math.Min(1160, work.Width) / 2);
        return height / 2 + Math.Min(60, height / 6);
    }

    class Fixture : Form {
        public Fixture(int index) {
            Text = "Rustile fixture " + index;
            var work = Screen.PrimaryScreen.WorkingArea;
            StartPosition = FormStartPosition.Manual;
            Bounds = new Rectangle(work.X + 30 + index * 28, work.Y + 25 + index * 25, 320 + index * 15, 230);
            if (index == 5) MinimumSize = new Size(160, TallMinimum(work));
            int corners = FixtureCorners[index % FixtureCorners.Length];
            Require(DwmSetWindowAttribute(Handle, WindowCornerPreference, ref corners, sizeof(int)) == 0, "Cannot configure fixture corners");
        }
        protected override void WndProc(ref Message m) {
            if (m.Msg == ShowFixtureDialog) {
                MessageBox.Show(this, "Owned modal window", "Rustile fixture dialog");
            } else if (m.Msg == RequestFocus) {
                Activate();
            } else {
                base.WndProc(ref m);
            }
        }
    }

    sealed class Child : IDisposable {
        readonly IntPtr process;
        public readonly int Pid;
        public readonly int ThreadId;
        public Child(string exe, string args, string desktop, bool launchHidden = false) {
            var startup = new StartupInfo { cb = Marshal.SizeOf(typeof(StartupInfo)), desktop = desktop,
                flags = launchHidden ? 1 /* STARTF_USESHOWWINDOW */ : 0, show = 0 /* SW_HIDE */ };
            ProcessInfo info;
            var line = new StringBuilder("\"" + exe + "\" " + args);
            if (!CreateProcess(exe, line, IntPtr.Zero, IntPtr.Zero, false, 0x08000000,
                IntPtr.Zero, null, ref startup, out info)) throw new Win32Exception();
            process = info.process;
            Pid = info.pid;
            ThreadId = info.tid;
            CloseHandle(info.thread);
        }
        public bool Exited(uint timeout) { return WaitForSingleObject(process, timeout) == 0; }
        public uint ExitCode {
            get { uint code; if (!GetExitCodeProcess(process, out code)) throw new Win32Exception(); return code; }
        }
        public void Send(uint message, int id, IntPtr target = default(IntPtr)) {
            if (!PostThreadMessage(ThreadId, message, (IntPtr)id, target)) throw new Win32Exception();
        }
        public void Dispose() {
            if (!Exited(0)) { TerminateProcess(process, 1); Exited(5000); }
            CloseHandle(process);
        }
    }

    static void Require(bool condition, string message) {
        if (!condition) throw new InvalidOperationException(message);
    }
    static void Until(string description, Func<bool> condition) {
        var timer = Stopwatch.StartNew();
        while (!condition()) {
            if (timer.ElapsedMilliseconds > 5000) throw new TimeoutException(description);
            Thread.Sleep(30);
        }
    }
    static void Message(IntPtr hwnd, uint msg) {
        if (!PostMessage(hwnd, msg, IntPtr.Zero, IntPtr.Zero)) throw new Win32Exception();
    }
    static IntPtr Find(int pid, bool dialog) {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr hwnd, IntPtr unused) {
            int owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner != pid || !IsWindowVisible(hwnd)) return true;
            var name = new StringBuilder(256);
            GetClassName(hwnd, name, name.Capacity);
            if (dialog ? name.ToString() == "#32770" : name.ToString().StartsWith("WindowsForms10.Window.")) {
                found = hwnd;
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    static Rectangle Bounds(IntPtr window) {
        Rect r;
        Require(GetWindowRect(window, out r), "Cannot read fixture window bounds");
        return Rectangle.FromLTRB(r.Left, r.Top, r.Right, r.Bottom);
    }
    static bool Tiled(Rectangle area, IntPtr[] windows) {
        var rectangles = windows.Select(Bounds).ToArray();
        var tolerance = Rectangle.Inflate(area, 20, 20); // allows invisible non-client resize frames
        long total = 0;
        foreach (var tile in rectangles) {
            if (!tolerance.Contains(tile)) return false;
            total += (long)tile.Width * tile.Height;
        }
        for (int i = 0; i < rectangles.Length; i++)
            for (int j = 0; j < i; j++) {
                var overlap = Rectangle.Intersect(rectangles[i], rectangles[j]);
                if (overlap.Width > 20 && overlap.Height > 20) return false;
            }
        return total >= (long)area.Width * area.Height * 80 / 100;
    }
    static int CornerPreference(IntPtr hwnd) {
        int value;
        Require(DwmGetWindowAttribute(hwnd, WindowCornerPreference, out value, sizeof(int)) == 0, "Cannot read DWM corner preference");
        return value;
    }
    static Rectangle VisibleFrame(IntPtr hwnd) {
        Rect frame;
        Require(DwmGetWindowAttribute(hwnd, ExtendedFrameBounds, out frame, Marshal.SizeOf(typeof(Rect))) == 0, "Cannot read DWM frame");
        return Rectangle.FromLTRB(frame.Left, frame.Top, frame.Right, frame.Bottom);
    }
    static IntPtr TrayWindow(Child manager) {
        IntPtr tray = IntPtr.Zero;
        Until("Rustile should create its tray callback window", () => {
            Require(!manager.Exited(0), "Rustile exited before its tray window appeared");
            EnumWindows(delegate(IntPtr hwnd, IntPtr unused) {
                int pid;
                GetWindowThreadProcessId(hwnd, out pid);
                if (pid != manager.Pid) return true;
                var cls = new StringBuilder(80);
                GetClassName(hwnd, cls, cls.Capacity);
                if (cls.ToString() != "Rustile.Tray") return true;
                tray = hwnd;
                return false;
            }, IntPtr.Zero);
            return tray != IntPtr.Zero;
        });
        return tray;
    }
    static IntPtr TrayButton(IntPtr tray, string label) {
        IntPtr button = IntPtr.Zero;
        Until("Tray button should be visible: " + label, () => {
            EnumChildWindows(tray, delegate(IntPtr hwnd, IntPtr unused) {
                if (!IsWindowVisible(hwnd)) return true;
                var cls = new StringBuilder(40);
                GetClassName(hwnd, cls, cls.Capacity);
                if (!String.Equals(cls.ToString(), "Button", StringComparison.OrdinalIgnoreCase)) return true;
                var text = new StringBuilder(120);
                UIntPtr length;
                if (SendMessageTimeout(hwnd, 0x000d, (UIntPtr)text.Capacity, text, 2, 500, out length) == IntPtr.Zero)
                    return true;
                var name = text.ToString();
                if (name != label && !name.StartsWith(label + ",", StringComparison.Ordinal)) return true;
                button = hwnd;
                return false;
            }, IntPtr.Zero);
            return button != IntPtr.Zero;
        });
        return button;
    }
    static void TrayClick(IntPtr tray, string label) {
        var button = TrayButton(tray, label);
        Require(IsWindowEnabled(button), "Tray button is disabled: " + label);
        Message(button, 0x00f5); // BM_CLICK
    }
    static void TrayKey(IntPtr tray, string label, Keys key) {
        var button = TrayButton(tray, label);
        if (!PostMessage(button, 0x0100 /* WM_KEYDOWN */, (IntPtr)(int)key, IntPtr.Zero))
            throw new Win32Exception();
    }
    static void TrayFocus(Child manager, IntPtr tray, string label) {
        var button = TrayButton(tray, label);
        var info = new GuiThreadInfo { size = Marshal.SizeOf(typeof(GuiThreadInfo)) };
        Until("Keyboard focus should move to " + label, () => {
            if (!GetGUIThreadInfo(manager.ThreadId, ref info)) throw new Win32Exception();
            return info.focus == button;
        });
    }
    static void TrayOpen(IntPtr tray, int activation, Rectangle screen) {
        // Version 4 shell callbacks carry the screen coordinates in wParam
        // and the icon ID (high word) plus activation event in lParam.
        int point = ((screen.Bottom - 12) << 16) | ((screen.Right - 12) & 0xffff);
        if (!PostMessage(tray, TrayCallback, (IntPtr)point, (IntPtr)((1 << 16) | activation)))
            throw new Win32Exception();
        TrayButton(tray, "Мозаика");
        Require(Screen.FromHandle(tray).WorkingArea.Contains(Bounds(tray)), "The tray popup extends outside its monitor's work area");
    }
    static void TrayChoose(IntPtr tray, int activation, Rectangle area, params string[] labels) {
        TrayOpen(tray, activation, area);
        foreach (var label in labels) TrayClick(tray, label);
        Until("Choosing a tray command should dismiss the flyout", () => !IsWindowVisible(tray));
    }
    static bool HasSpacing(IntPtr[] pair, Rectangle work) {
        int dpi = (int)GetDpiForSystem();
        int padding = (12 * dpi + 48) / 96, gap = (16 * dpi + 48) / 96;
        var a = VisibleFrame(pair[0]);
        var b = VisibleFrame(pair[1]);
        int distance = Math.Max(Math.Max(a.Left - b.Right, b.Left - a.Right),
            Math.Max(a.Top - b.Bottom, b.Top - a.Bottom));
        return Rectangle.Union(a, b) == Rectangle.Inflate(work, -padding, -padding) && distance == gap;
    }
    static void TraySettings(Child manager, string rustile, string desktop, List<Child> children,
        IntPtr[] pair, Rectangle[] originals, Rectangle area) {
        var tray = TrayWindow(manager);
        TrayOpen(tray, TrayKeyboard, area);
        TrayFocus(manager, tray, "Мозаика");
        TrayKey(tray, "Мозаика", Keys.Down);
        TrayFocus(manager, tray, "Горячие клавиши");
        TrayKey(tray, "Горячие клавиши", Keys.Escape);
        Until("Escape should dismiss the first popup after a hidden launch", () => !IsWindowVisible(tray));
        TrayChoose(tray, TraySelect, area, "Между окнами", "16 px");
        TrayChoose(tray, TrayContext, area, "По краям экрана", "12 px");
        Until("The selected tray spacing should reach the DWM frames", () => HasSpacing(pair, area));
        TrayOpen(tray, TrayKeyboard, area);
        TrayKey(tray, "Толщина обводки", Keys.Right);
        TrayFocus(manager, tray, "1 px");
        TrayKey(tray, "3 px", Keys.Enter);
        Until("Choosing a thickness with Enter should dismiss the flyout", () => !IsWindowVisible(tray));
        TrayOpen(tray, TraySelect, area);
        TrayClick(tray, "Горячие клавиши");
        TrayClick(tray, "Перевести фокус");
        Require(!IsWindowEnabled(TrayButton(tray, "Win+Shift+стрелки")), "A duplicate move binding was offered");
        TrayClick(tray, "Alt+стрелки");
        Until("Choosing a shortcut should dismiss the flyout", () => !IsWindowVisible(tray));
        Console.WriteLine("PASS  Left, right and keyboard tray callbacks; live inner/outer spacing, outline option and conflict-aware shortcuts");

        var tiled = pair.Select(VisibleFrame).ToArray();
        TrayChoose(tray, TrayContext, area, "Мозаика");
        TrayOpen(tray, TraySelect, area);
        TrayButton(tray, "Мозаика, Выключена");
        Require(pair.Select(VisibleFrame).SequenceEqual(tiled), "Pause moved a tiled window");
        TrayClick(tray, "Вернуть исходные позиции");
        Until("Explicit tray restore should recover both originals", () => pair.Select(Bounds).SequenceEqual(originals));
        TrayOpen(tray, TrayKeyboard, area);
        TrayClick(tray, "Выйти из Rustile");
        Require(manager.Exited(5000) && manager.ExitCode == 0, "Tray exit failed");

        var restarted = new Child(rustile, "", desktop, true);
        children.Add(restarted);
        tray = TrayWindow(restarted);
        TrayOpen(tray, TrayContext, area);
        TrayButton(tray, "Мозаика, Выключена");
        TrayButton(tray, "Толщина обводки, 3 px");
        Require(pair.Select(Bounds).SequenceEqual(originals), "Disabled startup tiled the windows");
        TrayClick(tray, "Горячие клавиши");
        TrayButton(tray, "Перевести фокус, Alt+стрелки");
        TrayKey(tray, "Перевести фокус, Alt+стрелки", Keys.Left);
        TrayButton(tray, "Мозаика, Выключена");
        TrayFocus(restarted, tray, "Горячие клавиши");
        TrayClick(tray, "Мозаика");
        Until("Resume should use the stored inner and outer gaps", () => HasSpacing(pair, area));
        TrayOpen(tray, TraySelect, area);
        TrayClick(tray, "Выйти из Rustile");
        Require(restarted.Exited(5000) && restarted.ExitCode == 0, "Second tray exit failed");
        Until("Exit after resume should restore the original frames", () => pair.Select(Bounds).SequenceEqual(originals));
        Console.WriteLine("PASS  Tray pause, explicit restore, paused restart, restored shortcut/outline/spacing, resume and tray exit");
    }
    // Direction encodings match the WM_APP command sent by the shortcut hook.
    static int NeighborDirection(Rectangle origin, Rectangle other) {
        if (Math.Min(origin.Bottom, other.Bottom) > Math.Max(origin.Top, other.Top)) {
            if (other.Right <= origin.Left) return 0;
            if (origin.Right <= other.Left) return 1;
        }
        if (Math.Min(origin.Right, other.Right) > Math.Max(origin.Left, other.Left)) {
            if (other.Bottom <= origin.Top) return 2;
            if (origin.Bottom <= other.Top) return 3;
        }
        return -1;
    }
    static void MoveOnHiddenDesktop(Child manager, IntPtr[] windows) {
        var active = GetForegroundWindow();
        int source = Array.IndexOf(windows, active);
        Require(source >= 0, "Move needs a foreground fixture");
        var before = windows.Select(VisibleFrame).ToArray();
        var from = before[source];
        int direction = before.Select(r => NeighborDirection(from, r)).First(dir => dir >= 0);
        manager.Send(RustileShortcut, 50 + direction, active);
        Until("Move should change both native frames", () => windows.Select(VisibleFrame)
            .Where((r, i) => r != before[i]).Count() == 2);
        var immediate = windows.Select(VisibleFrame).ToArray();
        int peer = Enumerable.Range(0, windows.Length).Single(i => i != source && immediate[i] != before[i]);
        Require(NeighborDirection(from, before[peer]) == direction, "Move selected a window outside that direction");
        Require(!immediate[source].Contains(from) && immediate[source].IntersectsWith(before[peer]),
            "The first move must leave part of the original tile and enter the visible neighbor's tile");
        var expected = (Rectangle[])before.Clone();
        expected[source] = before[peer];
        expected[peer] = from;
        if (!windows.Select(VisibleFrame).SequenceEqual(expected))
            manager.Send(RustileShortcut, 50 + direction, active);
        Until("Move should exchange exactly the selected slots",
            () => windows.Select(VisibleFrame).SequenceEqual(expected));
        Require(GetForegroundWindow() == active, "Move changed keyboard focus");
    }
    static bool ResizeOnHiddenDesktop(Child manager, IntPtr[] handles) {
        // A hidden desktop may have no foreground window. Do not transfer
        // foreground or fake physical input across desktop boundaries.
        Message(handles[0], RequestFocus);
        Thread.Sleep(120);
        var active = GetForegroundWindow();
        if (!handles.Contains(active)) return false;
        var before = handles.Select(Bounds).ToArray();
        manager.Send(RustileShortcut, 1, active);
        Thread.Sleep(60);
        // At least one of each opposite pair points at an internal divider.
        int moved = -1;
        foreach (int direction in new[] { 20, 21, 22, 23 }) {
            manager.Send(RustileShortcut, direction, active);
            Thread.Sleep(60);
            if (!handles.Select(Bounds).SequenceEqual(before)) { moved = direction; break; }
        }
        Require(moved >= 0, "Resize command did not move a divider");
        manager.Send(RustileShortcut, moved ^ 1, active);
        Until("Opposite plain arrow should return the divider", () => handles.Select(Bounds).SequenceEqual(before));
        manager.Send(RustileShortcut, moved, active);
        Until("Resize should move the divider again", () => !handles.Select(Bounds).SequenceEqual(before));
        manager.Send(RustileShortcut, 41, active);
        Until("Escape should restore the initial split", () => handles.Select(Bounds).SequenceEqual(before));
        manager.Send(RustileShortcut, 1, active);
        Thread.Sleep(60);
        foreach (int direction in new[] { 20, 21, 22, 23 }) {
            manager.Send(RustileShortcut, direction, active);
            Thread.Sleep(60);
            if (!handles.Select(Bounds).SequenceEqual(before)) break;
        }
        var committed = handles.Select(Bounds).ToArray();
        Require(!before.SequenceEqual(committed), "Second resize did not run");
        manager.Send(RustileShortcut, 40, active);
        manager.Send(RustileShortcut, 41, active);
        Thread.Sleep(120);
        Require(handles.Select(Bounds).SequenceEqual(committed), "Enter did not commit the split");
        return true;
    }
    static void Run(string rustile, string managerTests) {
        var desktopName = "rustile-smoke-" + Guid.NewGuid().ToString("N");
        var originalDesktop = GetThreadDesktop(GetCurrentThreadId());
        // Never request DESKTOP_SWITCHDESKTOP permission.
        var desktop = CreateDesktop(desktopName, IntPtr.Zero, IntPtr.Zero, 0, 0x007f, IntPtr.Zero);
        if (desktop == IntPtr.Zero) throw new Win32Exception();
        var children = new List<Child>();
        var oldAppData = Environment.GetEnvironmentVariable("LOCALAPPDATA");
        var appData = Path.Combine(Path.GetDirectoryName(Process.GetCurrentProcess().MainModule.FileName), "local-appdata");
        bool ownsShortcut = false;
        const int fixtureShortcut = 0x5119;
        try {
            if (!SetThreadDesktop(desktop)) throw new Win32Exception();
            Directory.CreateDirectory(appData);
            Environment.SetEnvironmentVariable("LOCALAPPDATA", appData, EnvironmentVariableTarget.Process);
            var self = Process.GetCurrentProcess().MainModule.FileName;
            var windows = new IntPtr[3];
            var originals = new Rectangle[3];
            var fixtures = new Child[3];
            Action<int> launch = delegate(int index) {
                var fixture = new Child(self, "fixture " + index, desktopName);
                children.Add(fixture);
                fixtures[index] = fixture;
                Until("Fixture should create its window", () => (windows[index] = Find(fixture.Pid, false)) != IntPtr.Zero);
                originals[index] = Bounds(windows[index]);
            };
            launch(0); launch(1);
            // Exercise the default Windows modifier even when another application
            // has already registered Win+Enter on this desktop.
            ownsShortcut = RegisterHotKey(IntPtr.Zero, fixtureShortcut, 8, 13);
            if (!ownsShortcut)
                Require(Marshal.GetLastWin32Error() == 1409, "Could not establish a Win+Enter conflict");
            var manager = new Child(rustile, "", desktopName, true);
            children.Add(manager);
            var area = Screen.PrimaryScreen.WorkingArea;
            Until("Existing windows should tile on launch", () => {
                Require(!manager.Exited(0), "Rustile stopped before the initial layout");
                return Tiled(area, windows.Take(2).ToArray());
            });
            Console.WriteLine("PASS  Default Windows-key profile starts and tiles with Win+Enter already registered");
            var startupTiles = windows.Take(2).Select(VisibleFrame).ToArray();
            int anchor = Array.IndexOf(windows.Take(2).ToArray(), GetForegroundWindow());
            if (anchor < 0) {
                // A private desktop usually has no OS foreground. Feed the real
                // WinEvent hook a focus notification without activating anything.
                // With no focus Rustile takes the largest tile (last slot on
                // ties). Choose the other tile so an ignored event fails here.
                anchor = Enumerable.Range(0, 2)
                    .OrderBy(i => (long)startupTiles[i].Width * startupTiles[i].Height)
                    .ThenBy(i => startupTiles[i].Top).ThenBy(i => startupTiles[i].Left).First();
                NotifyWinEvent(3 /* EVENT_SYSTEM_FOREGROUND */, windows[anchor], 0, 0);
            }
            launch(2);
            Until("Newly opened fixture should receive a tile", () => Tiled(area, windows));
            Until("New fixture should split only the previous foreground tile", () =>
                VisibleFrame(windows[1 - anchor]) == startupTiles[1 - anchor] &&
                Rectangle.Union(VisibleFrame(windows[anchor]), VisibleFrame(windows[2])) == startupTiles[anchor]);
            Console.WriteLine("PASS  New fixture from the WinEvent hook splits the focused tile and preserves its neighbor");
            int outerGap = (int)((8 * GetDpiForSystem() + 48) / 96);
            var inset = Rectangle.Inflate(area, -outerGap, -outerGap);
            Until("DWM frames should leave the scaled outer gutter", () =>
                windows.Select(VisibleFrame).Aggregate(Rectangle.Union) == inset);
            Require(windows.Select(CornerPreference).SequenceEqual(FixtureCorners), "Tiling changed an app's corner preference");
            Console.WriteLine("PASS  Outer gutter and native round, small, and square corner preferences preserved");

            bool resized = ResizeOnHiddenDesktop(manager, windows);
            Console.WriteLine(resized ? "PASS  Shortcut dispatch, opposite arrows, Escape and Enter"
                : "SKIP  Focus-dependent resize commands: no foreground fixture on this hidden desktop");
            if (resized) {
                MoveOnHiddenDesktop(manager, windows);
                Console.WriteLine("PASS  Foreground move reaches the neighbor's slot, preserving all other frames");
            } else {
                Console.WriteLine("SKIP  Foreground shortcut dispatch for move: no foreground fixture on this hidden desktop");
            }
            var beforeDialog = windows.Select(Bounds).ToArray();
            Message(windows[0], ShowFixtureDialog);
            IntPtr dialog = IntPtr.Zero;
            Until("Modal dialog should be visible", () => (dialog = Find(fixtures[0].Pid, true)) != IntPtr.Zero);
            var dialogBounds = Bounds(dialog);
            Thread.Sleep(250);
            Require(GetWindow(dialog, 4) == windows[0], "Wrong modal dialog owner");
            Require(Bounds(dialog) == dialogBounds && dialogBounds.Width < area.Width / 2, "Dialog was resized");
            Require(windows.Select(Bounds).SequenceEqual(beforeDialog), "Opening a dialog rearranged the tiles");
            Message(dialog, WmClose);
            Until("Modal dialog should close", () => Find(fixtures[0].Pid, true) == IntPtr.Zero);
            Require(windows.Select(Bounds).SequenceEqual(beforeDialog), "Closing a dialog rearranged the tiles");
            Console.WriteLine("PASS  Owned #32770 dialog preserved, other tiles unchanged");

            Message(windows[2], WmClose);
            Until("Remaining tiles should reclaim the closed window's area", () =>
                fixtures[2].Exited(0) && Tiled(area, windows.Take(2).ToArray()));
            if (!resized)
                Until("Sibling should reclaim the exact original tile", () =>
                    windows.Take(2).Select(VisibleFrame).SequenceEqual(startupTiles));
            Console.WriteLine("PASS  Closing the third window reclaims its tile");
            TraySettings(manager, rustile, desktopName, children, windows.Take(2).ToArray(),
                originals.Take(2).ToArray(), area);
            Require(windows.Take(2).Select(CornerPreference).SequenceEqual(FixtureCorners.Take(2)), "Exit changed the fixtures' corners");

            Array.Resize(ref windows, 6);
            Array.Resize(ref originals, 6);
            Array.Resize(ref fixtures, 6);
            launch(2); launch(3); launch(4); launch(5);
            Environment.SetEnvironmentVariable("RUSTILE_SMOKE_DESKTOP", desktopName);
            Environment.SetEnvironmentVariable("RUSTILE_SMOKE_MIN_HEIGHT", TallMinimum(area).ToString());
            Environment.SetEnvironmentVariable("RUSTILE_SMOKE_WINDOWS", string.Join(" ", windows.Select(w => w.ToInt64())));
            // The hidden process has no console. Surface a failing Rust geometry
            // assertion in the smoke output, including actual and expected frames.
            string failureFile = Path.ChangeExtension(self, ".log");
            Environment.SetEnvironmentVariable("RUSTILE_SMOKE_FAILURE", failureFile);
            try {
                var geometry = new Child(managerTests, "--exact manager::desktop_test::move_windows --ignored", desktopName);
                children.Add(geometry);
                Require(geometry.Exited(20000), "Hidden geometry test timed out");
                Require(geometry.ExitCode == 0, File.Exists(failureFile)
                    ? File.ReadAllText(failureFile) : "Explicit-window move test failed on the hidden desktop");
            } finally {
                File.Delete(failureFile);
            }
            Require(GetProp(windows[0], "Rustile.Smoke.MovePassed") != IntPtr.Zero, "Manager test did not run");
            Until("Manager should restore all six fixtures", () => windows.Select(Bounds).SequenceEqual(originals));
            Console.WriteLine("PASS  Five real windows: three plus two slots, level DWM heights after resize and horizontal moves");
            Console.WriteLine(windows.Any(w => GetProp(w, "Rustile.Smoke.StaggeredPassed") != IntPtr.Zero)
                ? "PASS  Staggered Down shares the lower tile with backfill, then takes it; Up shares the L before completing the return"
                : "SKIP  Staggered preview: the remainder is below the application minimum on this display");
            Console.WriteLine("PASS  Six real windows: application minimum height, filled rows, rapid moves and resize, exact restore");
            Console.WriteLine("PASS  Foreground before SHOW, alternating local splits, unchanged background focus, and local reclaim");
            Console.WriteLine("PASS  Two Lefts from the lower-right tile share then take the tall neighbor's column, preserving its minimum");
            Console.WriteLine("PASS  Move Down at the bottom edge places Explorer below its tall neighbor and preserves the right column");
            Console.WriteLine("PASS  After Down then Right, the visible neighbor is above the active window; the next Right completes the move");
            Console.WriteLine("PASS  Three columns: Left puts the chat above the browser, then takes its column; Explorer keeps its frame");
            Console.WriteLine("PASS  A single Left takes the browser's column when the apps cannot stack");
            Console.WriteLine("PASS  Six windows: Up enters the aligned half of the upper tile; another Up takes that tile and restores the lower neighbor");
        } finally {
            children.Reverse();
            foreach (var child in children) child.Dispose();
            Environment.SetEnvironmentVariable("LOCALAPPDATA", oldAppData, EnvironmentVariableTarget.Process);
            if (ownsShortcut) UnregisterHotKey(IntPtr.Zero, fixtureShortcut);
            SetThreadDesktop(originalDesktop);
            CloseDesktop(desktop);
        }
    }
    [STAThread]
    static int Main(string[] args) {
        try {
            SetProcessDpiAwarenessContext((IntPtr)(-4));
            if (args.Length == 2 && args[0] == "fixture") Application.Run(new Fixture(int.Parse(args[1])));
            else if (args.Length == 2) {
                // A WinForms STA may already own an OLE window, which prevents
                // switching its desktop. Start a clean worker before using USER.
                Exception failure = null;
                var worker = new Thread(delegate() {
                    try { Run(args[0], args[1]); } catch (Exception error) { failure = error; }
                });
                worker.Start();
                worker.Join();
                if (failure != null) throw failure;
            }
            else throw new ArgumentException("Expected paths to rustile.exe and its Rust test binary");
            return 0;
        } catch (Exception error) {
            Console.Error.WriteLine(error);
            return 1;
        }
    }
}
