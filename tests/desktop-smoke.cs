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
    const int ToggleMosaic = 4;
    const uint TrayCallback = 0x8000 + 20;
    const int TraySelect = 0x400, TrayContext = 0x7b, TrayKeyboard = 0x401;
    const int ShowFixtureDialog = 0x8001, RequestFocus = 0x8002;
    const int ReadGeometryChanges = 0x8003, SetFixtureCloak = 0x8004;
    const int SetFixtureMode = 0x8005;
    enum FixtureMode { Normal, Borderless, Maximize, AppBounds, RestoreNormalStyle }
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
    [DllImport("advapi32.dll", SetLastError = true)] static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true)] static extern bool GetTokenInformation(IntPtr token,
        int infoClass, out int value, int size, out int written);
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
    [DllImport("user32.dll")] static extern int GetWindowLongW(IntPtr hwnd, int index);
    [DllImport("user32.dll")] static extern int SetWindowLongW(IntPtr hwnd, int index, int value);
    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after,
        int x, int y, int w, int h, uint flags);
    [DllImport("user32.dll")] static extern bool IsZoomed(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool ScreenToClient(IntPtr hwnd, ref Point point);
    [DllImport("user32.dll")] static extern IntPtr ChildWindowFromPointEx(IntPtr hwnd, Point point, uint flags);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool IsWindowEnabled(IntPtr hwnd);
    [DllImport("user32.dll")] static extern int GetDlgCtrlID(IntPtr hwnd);
    [DllImport("user32.dll")] static extern IntPtr GetDlgItem(IntPtr hwnd, int id);
    [DllImport("user32.dll")] static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr hwnd);
    [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr hwnd, IntPtr dc);
    [DllImport("gdi32.dll")] static extern uint GetPixel(IntPtr dc, int x, int y);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint msg, UIntPtr wp, StringBuilder text, uint flags,
        uint timeout, out UIntPtr result);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint msg, IntPtr wp, IntPtr lp, uint flags,
        uint timeout, out UIntPtr result);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr GetProp(IntPtr hwnd, string name);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr RemoveProp(IntPtr hwnd, string name);
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
    static Size DragPeerMinimum(Rectangle work) {
        return new Size(Math.Min(300, work.Width / 4), Math.Min(200, work.Height / 4));
    }

    class Fixture : Form {
        Rectangle lastBounds;
        readonly Rectangle normalBounds;
        int normalStyle;
        int geometryChanges;
        public Fixture(int index, bool dragPeer) {
            Text = "Rustile fixture " + index;
            var work = Screen.PrimaryScreen.WorkingArea;
            StartPosition = FormStartPosition.Manual;
            Bounds = new Rectangle(work.X + 30 + index * 28, work.Y + 25 + index * 25, 320 + index * 15, 230);
            if (index == 5) MinimumSize = new Size(160, TallMinimum(work));
            if (dragPeer) MinimumSize = DragPeerMinimum(work);
            int corners = FixtureCorners[index % FixtureCorners.Length];
            Require(DwmSetWindowAttribute(Handle, WindowCornerPreference, ref corners, sizeof(int)) == 0, "Cannot configure fixture corners");
            normalBounds = DesktopSmoke.Bounds(Handle);
        }
        bool ApplyMode(FixtureMode mode) {
            var bounds = normalBounds;
            uint flags = 0x14; // SWP_NOZORDER | SWP_NOACTIVATE
            switch (mode) {
                case FixtureMode.Borderless:
                    normalStyle = GetWindowLongW(Handle, -16 /* GWL_STYLE */);
                    SetWindowLongW(Handle, -16, (normalStyle & ~0x00cf0000 /* WS_OVERLAPPEDWINDOW */)
                        | unchecked((int)0x80000000) /* WS_POPUP */);
                    bounds = Screen.FromHandle(Handle).Bounds;
                    flags |= 0x20; // SWP_FRAMECHANGED
                    break;
                case FixtureMode.Maximize:
                    WindowState = FormWindowState.Maximized;
                    return IsZoomed(Handle);
                case FixtureMode.Normal:
                    if (IsZoomed(Handle)) {
                        WindowState = FormWindowState.Normal;
                    } else {
                        SetWindowLongW(Handle, -16, normalStyle);
                        flags |= 0x20;
                    }
                    break;
                case FixtureMode.RestoreNormalStyle:
                    // No application move, resize or foreground notification.
                    SetWindowLongW(Handle, -16, normalStyle);
                    return GetWindowLongW(Handle, -16) == normalStyle;
                case FixtureMode.AppBounds:
                    break;
            }
            return SetWindowPos(Handle, IntPtr.Zero, bounds.X, bounds.Y, bounds.Width, bounds.Height, flags)
                && (mode != FixtureMode.AppBounds || DesktopSmoke.Bounds(Handle) == normalBounds);
        }
        protected override void OnFormClosing(FormClosingEventArgs e) {
            if (RemoveProp(Handle, "Rustile.Smoke.VetoNextClose") != IntPtr.Zero)
                e.Cancel = true;
            base.OnFormClosing(e);
        }
        protected override void WndProc(ref Message m) {
            if (m.Msg == ShowFixtureDialog) {
                MessageBox.Show(this, "Owned modal window", "Rustile fixture dialog");
            } else if (m.Msg == RequestFocus) {
                Activate();
            } else if (m.Msg == ReadGeometryChanges) {
                m.Result = (IntPtr)geometryChanges;
            } else if (m.Msg == SetFixtureCloak) {
                int value = m.WParam == IntPtr.Zero ? 0 : 1;
                m.Result = (IntPtr)DwmSetWindowAttribute(Handle, 13 /* DWMWA_CLOAK */, ref value, sizeof(int));
            } else if (m.Msg == SetFixtureMode) {
                m.Result = ApplyMode((FixtureMode)m.WParam.ToInt32()) ? (IntPtr)1 : IntPtr.Zero;
            } else {
                if (m.Msg == 0x112 /* WM_SYSCOMMAND */ && (m.WParam.ToInt64() & 0xfff0) == 0xf030 /* SC_MAXIMIZE */)
                    RemoveProp(m.HWnd, "Rustile.Smoke.ExpectNoMaximize");
                base.WndProc(ref m);
                if (m.Msg == 0x47 /* WM_WINDOWPOSCHANGED */) {
                    Rect rect;
                    if (GetWindowRect(m.HWnd, out rect)) {
                        var bounds = Rectangle.FromLTRB(rect.Left, rect.Top, rect.Right, rect.Bottom);
                        if (bounds != lastBounds) {
                            lastBounds = bounds;
                            geometryChanges++;
                        }
                    }
                }
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
        public bool IsElevated() {
            IntPtr token;
            if (!OpenProcessToken(process, 8 /* TOKEN_QUERY */, out token)) throw new Win32Exception();
            try {
                int elevated, written;
                if (!GetTokenInformation(token, 20 /* TokenElevation */, out elevated, sizeof(int), out written))
                    throw new Win32Exception();
                return elevated != 0;
            } finally { CloseHandle(token); }
        }
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
    static void SetMode(IntPtr hwnd, FixtureMode mode) {
        UIntPtr result;
        Require(SendMessageTimeout(hwnd, SetFixtureMode, (IntPtr)(int)mode, IntPtr.Zero,
            2 /* SMTO_ABORTIFHUNG */, 3000, out result) != IntPtr.Zero && result != UIntPtr.Zero,
            "Fixture failed to change mode: " + mode);
    }
    static bool PairFills(Rectangle area, int gap, IntPtr app, IntPtr peer) {
        var a = VisibleFrame(app);
        var b = VisibleFrame(peer);
        if (Rectangle.Union(a, b) != area) return false;
        if (a.Top == area.Top && b.Top == area.Top && a.Height == area.Height && b.Height == area.Height)
            return Math.Max(a.Left, b.Left) - Math.Min(a.Right, b.Right) == gap;
        return a.Left == area.Left && b.Left == area.Left && a.Width == area.Width && b.Width == area.Width
            && Math.Max(a.Top, b.Top) - Math.Min(a.Bottom, b.Bottom) == gap;
    }
    static void SetFixtureFrame(IntPtr hwnd, Rectangle target) {
        var outer = Bounds(hwnd);
        var frame = VisibleFrame(hwnd);
        Require(SetWindowPos(hwnd, IntPtr.Zero,
            target.Left + outer.Left - frame.Left, target.Top + outer.Top - frame.Top,
            target.Width + outer.Width - frame.Width, target.Height + outer.Height - frame.Height,
            0x14 /* SWP_NOACTIVATE | SWP_NOZORDER */), "Cannot position fixture at " + target);
    }
    static void StackOnLeft(Rectangle area, int gap, IntPtr app, IntPtr peer) {
        int w = (area.Width - gap) / 2, h = (area.Height - gap) / 2;
        var top = new Rectangle(area.Left, area.Top, w, h);
        var bottom = Rectangle.FromLTRB(area.Left, top.Bottom + gap, top.Right, area.Bottom);
        SetFixtureFrame(app, top);
        SetFixtureFrame(peer, bottom);
        Require(VisibleFrame(app) == top && VisibleFrame(peer) == bottom, "Expected both frames in the left half");
    }
    static void FullscreenRoundTrips(string rustile, string desktop, List<Child> children,
        IntPtr app, IntPtr peer, Rectangle[] originals) {
        var screen = Screen.PrimaryScreen;
        int gap = (int)((8 * GetDpiForSystem() + 48) / 96);
        var tiledArea = Rectangle.Inflate(screen.WorkingArea, -gap, -gap);
        int owner;
        GetWindowThreadProcessId(peer, out owner);
        Message(peer, ShowFixtureDialog);
        IntPtr dialog = IntPtr.Zero;
        Until("Open the excluded owned dialog for the fullscreen test", () => (dialog = Find(owner, true)) != IntPtr.Zero);
        var dialogBounds = VisibleFrame(dialog);
        var beforeFocus = GetForegroundWindow();

        SetMode(app, FixtureMode.Borderless);
        Until("The borderless fixture must cover the monitor", () => VisibleFrame(app) == screen.Bounds);
        var manager = new Child(rustile, "", desktop, true);
        children.Add(manager);
        Until("Startup must exclude an existing fullscreen window and tile the other app", () =>
            VisibleFrame(peer) == tiledArea && VisibleFrame(app) == screen.Bounds);
        Console.WriteLine("PASS  Startup with an existing fullscreen app tiles its peer and leaves the fullscreen frame intact");

        bool enteredBeforeStartup = true;
        foreach (var mode in new[] { FixtureMode.Borderless, FixtureMode.Borderless, FixtureMode.Maximize }) {
            bool stackOnReturn = mode == FixtureMode.Borderless && !enteredBeforeStartup;
            if (!enteredBeforeStartup) SetMode(app, mode);
            if (mode == FixtureMode.Maximize) {
                Until("Native maximize must cover the work area", () => IsZoomed(app) && VisibleFrame(app).Contains(tiledArea));
                var maximized = VisibleFrame(app);
                Until("The fixture must maximize while its sibling reclaims the work area", () =>
                    IsZoomed(app) && VisibleFrame(app) == maximized && VisibleFrame(peer) == tiledArea);
            } else {
                Until("Fullscreen must remain at monitor bounds while its peer fills the work area", () =>
                    VisibleFrame(app) == screen.Bounds && VisibleFrame(peer) == tiledArea);
            }
            Require(VisibleFrame(dialog) == dialogBounds, "Fullscreen transition moved the excluded dialog");
            if (enteredBeforeStartup) {
                // Browser/video apps may shrink the popup first. The new
                // geometry arrives while the HWND is still ineligible.
                SetMode(app, FixtureMode.AppBounds);
                Thread.Sleep(230);
                Require(Bounds(app) == originals[0] && VisibleFrame(peer) == tiledArea
                    && (GetWindowLongW(app, -16) & 0x00cf0000) == 0, "The intermediate popup should stay excluded");
                SetMode(app, FixtureMode.RestoreNormalStyle);
                enteredBeforeStartup = false;
            } else {
                SetMode(app, FixtureMode.Normal);
            }
            Until("The returning app and its peer must exactly fill the work area: " + mode, () =>
                !IsZoomed(app) && PairFills(tiledArea, gap, app, peer));

            // The first request has reached both HWNDs. A later, unsnapped
            // application SetWindowPos emits a real LOCATIONCHANGE by itself.
            Thread.Sleep(400);
            if (stackOnReturn) StackOnLeft(tiledArea, gap, app, peer);
            else SetMode(app, FixtureMode.AppBounds);
            Until("Delayed app restore must rejoin both tiles without a focus event: " + mode, () =>
                PairFills(tiledArea, gap, app, peer));
            Thread.Sleep(150); // a stale DWM sample must not pass for a stable repair
            Require(PairFills(tiledArea, gap, app, peer), "The repaired tile frames drifted: " + mode);
            Require(VisibleFrame(dialog) == dialogBounds, "Restoring the app moved the excluded dialog");
            if (mode == FixtureMode.Borderless) {
                Require(GetForegroundWindow() == beforeFocus, "Borderless round trip changed foreground");
            }
        }
        manager.Send(0x12 /* WM_QUIT */, 0);
        Require(manager.Exited(5000) && manager.ExitCode == 0, "Fullscreen test manager failed to exit cleanly");
        Until("Fullscreen test manager must restore the two original windows", () =>
            Bounds(app) == originals[0] && Bounds(peer) == originals[1]);
        Message(dialog, WmClose);
        Until("Excluded dialog should close after fullscreen checks", () => Find(owner, true) == IntPtr.Zero);
        Console.WriteLine("PASS  Fullscreen and maximize/restore refill the work area after delayed app bounds or two windows stacked in its left half");
    }
    static IntPtr ManagerWindow(Child manager, string className, bool visibleOnly) {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr hwnd, IntPtr unused) {
            int pid;
            GetWindowThreadProcessId(hwnd, out pid);
            if (pid != manager.Pid || (visibleOnly && !IsWindowVisible(hwnd))) return true;
            var cls = new StringBuilder(80);
            GetClassName(hwnd, cls, cls.Capacity);
            if (cls.ToString() != className) return true;
            found = hwnd;
            return false;
        }, IntPtr.Zero);
        return found;
    }
    static IntPtr TrayWindow(Child manager, string className = "Rustile.Tray") {
        IntPtr tray = IntPtr.Zero;
        Until("Rustile should create " + className, () => {
            Require(!manager.Exited(0), "Rustile exited before " + className + " appeared");
            return (tray = ManagerWindow(manager, className, false)) != IntPtr.Zero;
        });
        return tray;
    }
    static IntPtr EditorControl(IntPtr editor, int id) {
        IntPtr result = IntPtr.Zero;
        EnumChildWindows(editor, delegate(IntPtr hwnd, IntPtr unused) {
            if (GetDlgCtrlID(hwnd) != id) return true;
            result = hwnd;
            return false;
        }, IntPtr.Zero);
        Require(result != IntPtr.Zero, "Missing shortcut editor control " + id);
        return result;
    }
    static string ControlText(IntPtr hwnd) {
        var text = new StringBuilder(256);
        UIntPtr result;
        if (SendMessageTimeout(hwnd, 0x000d, (UIntPtr)text.Capacity, text, 2, 1000, out result) == IntPtr.Zero)
            throw new Win32Exception();
        return text.ToString();
    }
    static IntPtr OpenShortcutEditor(Child manager, IntPtr tray, Rectangle area) {
        TrayClick(tray, "Горячие клавиши");
        var editor = TrayWindow(manager, "Rustile.Shortcuts");
        Until("The editor should open and the tray flyout should close", () =>
            IsWindowVisible(editor) && !IsWindowVisible(tray));
        var bounds = Bounds(editor);
        Require(Math.Abs((bounds.Left + bounds.Right) - (area.Left + area.Right)) < 8
            && Math.Abs((bounds.Top + bounds.Bottom) - (area.Top + area.Bottom)) < 8,
            "Shortcut editor did not open in the center of the monitor");
        return editor;
    }
    static void ClickControl(IntPtr window, int id) {
        SendTrayMessage(EditorControl(window, id), 0x00f5 /* BM_CLICK */, IntPtr.Zero, IntPtr.Zero);
    }
    static void BeginRecording(IntPtr row) {
        var position = (IntPtr)((10 << 16) | 20);
        SendTrayMessage(row, 0x0201 /* WM_LBUTTONDOWN */, (IntPtr)1, position);
        SendTrayMessage(row, 0x0202 /* WM_LBUTTONUP */, IntPtr.Zero, position);
        SendTrayMessage(row, 0x0203 /* WM_LBUTTONDBLCLK */, (IntPtr)1, position);
        SendTrayMessage(row, 0x0202 /* WM_LBUTTONUP */, IntPtr.Zero, position);
    }
    static void CaptureKey(IntPtr row, Keys key, bool down, bool system = false) {
        uint message = down ? 0x0100u : 0x0101u; // WM_KEYDOWN / WM_KEYUP
        if (system) message += 4; // WM_SYSKEYDOWN / WM_SYSKEYUP
        int flags = 1 | (system ? 1 << 29 : 0) | (down ? 0 : unchecked((int)0xc0000000));
        if (!PostMessage(row, message, (IntPtr)(int)key, (IntPtr)flags)) throw new Win32Exception();
    }
    static void EnterChord(IntPtr row, Keys modifier, Keys key) {
        bool system = modifier == Keys.Menu;
        CaptureKey(row, modifier, true, system);
        CaptureKey(row, key, true, system);
        CaptureKey(row, key, false, system);
        CaptureKey(row, modifier, false, system);
    }
    static string[] ShortcutLabels(IntPtr editor) {
        return Enumerable.Range(1000, 50).Select(id => ControlText(EditorControl(editor, id))).ToArray();
    }
    static void EmptyIdleHeader(IntPtr editor) {
        var status = GetDlgItem(editor, 506);
        Require(status == IntPtr.Zero || !IsWindowVisible(status) || ControlText(status).Length == 0,
            "Idle editor still shows a permanent hint or save message");
        var oldHeading = GetDlgItem(editor, 511);
        Require(oldHeading == IntPtr.Zero || !IsWindowVisible(oldHeading) || ControlText(oldHeading) != "Горячие клавиши",
            "The editor repeats its native window title");
    }
    enum ConflictChoice { DefaultEnter, TabEnter, Escape, ClickAssign }
    static void ChooseConflict(Child manager, IntPtr editor, ConflictChoice choice) {
        IntPtr dialog = IntPtr.Zero;
        Until("The assignment conflict should appear over its editor", () => {
            Require(!manager.Exited(0), "Rustile exited before showing the conflict");
            EnumWindows(delegate(IntPtr hwnd, IntPtr unused) {
                int pid;
                GetWindowThreadProcessId(hwnd, out pid);
                if (pid != manager.Pid || GetWindow(hwnd, 4 /* GW_OWNER */) != editor || !IsWindowVisible(hwnd))
                    return true;
                var cls = new StringBuilder(48);
                GetClassName(hwnd, cls, cls.Capacity);
                if (cls.ToString() != "#32770") return true;
                dialog = hwnd;
                return false;
            }, IntPtr.Zero);
            return dialog != IntPtr.Zero;
        });
        var copy = new StringBuilder(ControlText(dialog));
        EnumChildWindows(dialog, delegate(IntPtr child, IntPtr unused) {
            if (IsWindowVisible(child)) copy.Append(' ').Append(ControlText(child));
            return true;
        }, IntPtr.Zero);
        string message = copy.ToString();
        Require(new[] { "Фокус влево", "Переместить влево", "Win+←" }
            .All(label => message.IndexOf(label, StringComparison.OrdinalIgnoreCase) >= 0),
            "Conflict prompt must identify the chord and both actions: " + message);
        var cancel = EditorControl(dialog, 2 /* IDCANCEL */);
        var assign = EditorControl(dialog, 1 /* IDOK */);
        Require(ControlText(cancel) == "Отмена" && ControlText(assign) == "Переназначить",
            "The conflict dialog should offer cancel and reassign");
        int owner;
        int dialogThread = (int)GetWindowThreadProcessId(dialog, out owner);
        Func<IntPtr> focused = delegate {
            var info = new GuiThreadInfo { size = Marshal.SizeOf(typeof(GuiThreadInfo)) };
            if (!GetGUIThreadInfo(dialogThread, ref info)) throw new Win32Exception();
            return info.focus;
        };
        Until("Cancel must have the initial keyboard focus", () => focused() == cancel);
        if (choice == ConflictChoice.ClickAssign) {
            // BM_CLICK can silently fail on a dialog outside the input desktop.
            // Queue a click at the button center on the dialog's own message loop.
            var size = Bounds(assign).Size;
            var center = (IntPtr)(((size.Height / 2) << 16) | (size.Width / 2));
            if (!PostMessage(assign, 0x0201 /* WM_LBUTTONDOWN */, (IntPtr)1, center)
                || !PostMessage(assign, 0x0202 /* WM_LBUTTONUP */, IntPtr.Zero, center))
                throw new Win32Exception();
        } else if (choice == ConflictChoice.Escape) {
            CaptureKey(cancel, Keys.Escape, true);
        } else {
            var button = cancel;
            if (choice == ConflictChoice.TabEnter) {
                CaptureKey(cancel, Keys.Tab, true);
                Until("Tab should focus the reassign button", () => focused() == assign);
                CaptureKey(assign, Keys.Tab, false);
                button = assign;
            }
            CaptureKey(button, Keys.Enter, true);
            Thread.Sleep(60);
            Require(IsWindowVisible(dialog) && !IsWindowEnabled(editor), "Enter must wait for key release");
            CaptureKey(button, Keys.Enter, false);
        }
        Until("The conflict prompt should dismiss after " + choice,
            () => !IsWindowVisible(dialog) && IsWindowEnabled(editor));
    }
    static bool QuitChordAvailable() {
        const int probe = 0x5120;
        if (!RegisterHotKey(IntPtr.Zero, probe, 7 /* Ctrl+Alt+Shift */, (uint)Keys.Q)) {
            Require(Marshal.GetLastWin32Error() == 1409, "Cannot probe the former global quit shortcut");
            return false;
        }
        Require(UnregisterHotKey(IntPtr.Zero, probe), "Cannot release the quit shortcut probe");
        return true;
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
    static void ClickArrow(IntPtr tray, string label) {
        var arrow = TrayButton(tray, label);
        var bounds = Bounds(arrow);
        var point = new Point(bounds.Left + bounds.Width / 2, bounds.Top + bounds.Height / 2);
        var trayPoint = point;
        Require(ScreenToClient(tray, ref trayPoint), "Cannot locate tray arrow");
        var hit = ChildWindowFromPointEx(tray, trayPoint, 3 /* skip invisible and disabled */);
        Require(hit == arrow, "The pointer cannot reach " + label);
        Require(ScreenToClient(hit, ref point), "Cannot locate arrow center");
        var coords = (IntPtr)((point.Y << 16) | (point.X & 0xffff));
        if (!PostMessage(hit, 0x0201 /* WM_LBUTTONDOWN */, (IntPtr)1, coords)
            || !PostMessage(hit, 0x0202 /* WM_LBUTTONUP */, IntPtr.Zero, coords))
            throw new Win32Exception();
    }
    static void KeyDown(IntPtr control, Keys key) {
        if (!PostMessage(control, 0x0100 /* WM_KEYDOWN */, (IntPtr)(int)key, IntPtr.Zero))
            throw new Win32Exception();
    }
    static void TrayKey(IntPtr tray, string label, Keys key) { KeyDown(TrayButton(tray, label), key); }
    static void Wheel(IntPtr receiver, IntPtr hovered, int delta) {
        var bounds = Bounds(hovered);
        int x = bounds.Left + bounds.Width / 2, y = bounds.Top + bounds.Height / 2;
        if (!PostMessage(receiver, 0x020a /* WM_MOUSEWHEEL */, (IntPtr)(delta << 16),
                (IntPtr)((y << 16) | (x & 0xffff)))) throw new Win32Exception();
    }
    static IntPtr TypeNumber(IntPtr tray, string label, string value) {
        TrayClick(tray, label);
        var edit = IntPtr.Zero;
        int pid;
        int tid = (int)GetWindowThreadProcessId(tray, out pid);
        var info = new GuiThreadInfo { size = Marshal.SizeOf(typeof(GuiThreadInfo)) };
        Until("The inline edit should take focus for " + label, () => {
            if (!GetGUIThreadInfo(tid, ref info)) throw new Win32Exception();
            edit = info.focus;
            var cls = new StringBuilder(20);
            return edit != IntPtr.Zero && IsWindowVisible(edit) && GetClassName(edit, cls, cls.Capacity) > 0
                && String.Equals(cls.ToString(), "Edit", StringComparison.OrdinalIgnoreCase);
        });
        foreach (var digit in value) {
            if (!PostMessage(edit, 0x0102 /* WM_CHAR */, (IntPtr)(int)digit, IntPtr.Zero))
                throw new Win32Exception();
        }
        return edit;
    }
    static void EditNumber(IntPtr tray, string label, string value, Keys finish) {
        var edit = TypeNumber(tray, label, value);
        KeyDown(edit, finish);
        Until("Editing should finish inside the menu", () => !IsWindowVisible(edit) && IsWindowVisible(tray));
    }
    static void TrayFocus(IntPtr tray, string label) {
        var button = TrayButton(tray, label);
        int pid;
        int thread = (int)GetWindowThreadProcessId(tray, out pid);
        var info = new GuiThreadInfo { size = Marshal.SizeOf(typeof(GuiThreadInfo)) };
        Until("Keyboard focus should move to " + label, () => {
            if (!GetGUIThreadInfo(thread, ref info)) throw new Win32Exception();
            return info.focus == button;
        });
    }
    static void SendTrayMessage(IntPtr tray, uint message, IntPtr w, IntPtr l) {
        UIntPtr result;
        if (SendMessageTimeout(tray, message, w, l, 2 /* SMTO_ABORTIFHUNG */, 3000, out result) == IntPtr.Zero)
            throw new Win32Exception();
    }
    static void TrayNotify(IntPtr tray, int activation, Rectangle screen) {
        // Version 4 shell callbacks carry the screen coordinates in wParam
        // and the icon ID (high word) plus activation event in lParam.
        int point = ((screen.Bottom - 12) << 16) | ((screen.Right - 12) & 0xffff);
        SendTrayMessage(tray, TrayCallback, (IntPtr)point, (IntPtr)((1 << 16) | activation));
    }
    static void TrayClickWithDeactivation(IntPtr tray, int activation, Rectangle area) {
        bool right = activation == TrayContext;
        TrayNotify(tray, right ? 0x0204 : 0x0201 /* WM_*BUTTONDOWN */, area);
        SendTrayMessage(tray, 0x0006 /* WM_ACTIVATE, WA_INACTIVE */, IntPtr.Zero, IntPtr.Zero);
        Require(IsWindowVisible(tray), "The popup should stay visible until the icon click completes");
        TrayNotify(tray, right ? 0x0205 : 0x0202 /* WM_*BUTTONUP */, area);
        TrayNotify(tray, activation, area);
        Until("An icon click that takes activation should dismiss the popup once", () => !IsWindowVisible(tray));
    }
    static void TrayOpen(IntPtr tray, int activation, Rectangle screen) {
        TrayNotify(tray, activation, screen);
        // Sample the existing window surface immediately. PrintWindow, waiting
        // for a control, or sending WM_PAINT would hide the blank-first-frame bug.
        var header = GetDlgItem(tray, 100);
        Require(header != IntPtr.Zero && IsWindowVisible(header), "Missing visible tray header at open");
        int dpi = (int)GetDpiForWindow(header);
        int size = (20 * dpi + 48) / 96, left = (10 * dpi + 48) / 96;
        int top = Bounds(header).Height / 2 - size / 2;
        var dc = GetDC(header);
        Require(dc != IntPtr.Zero, "Cannot sample the first tray frame");
        try {
            // Two differently colored tiles within the logo, independent of
            // the current light, dark or high-contrast background.
            uint main = GetPixel(dc, left + size / 4, top + size / 2);
            uint upper = GetPixel(dc, left + size * 7 / 10, top + size / 4);
            Require(main != uint.MaxValue && upper != uint.MaxValue && main != upper,
                String.Format("Tray exposed an unpainted header on activation {0}: {1:X8}, {2:X8}", activation, main, upper));
        } finally { ReleaseDC(header, dc); }
        int cloaked;
        Require(DwmGetWindowAttribute(tray, 14 /* DWMWA_CLOAKED */, out cloaked, sizeof(int)) == 0
            && (cloaked & 1 /* DWM_CLOAKED_APP */) == 0, "Rustile left the tray popup cloaked");
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
    static void StartupSettings(string testBinary, string desktop, Rectangle area) {
        // Only this Rust test process redirects HKCU. Never send a startup
        // command to the production instance used by the other smoke tests.
        const string startup = "Запускать при входе в Windows", admin = "От имени администратора";
        using (var manager = new Child(testBinary, "--exact runtime::tests::isolated_startup_tray --ignored", desktop)) {
            var tray = TrayWindow(manager);
            try {
                TrayOpen(tray, TrayKeyboard, area);
                var compact = Bounds(tray);
                TrayButton(tray, startup + ", Выключено");
                TrayKey(tray, startup, Keys.Down);
                TrayFocus(tray, "Выйти из Rustile");
                TrayKey(tray, "Выйти из Rustile", Keys.Up);
                TrayFocus(tray, startup);
                TrayKey(tray, startup, Keys.Space);
                TrayButton(tray, startup + ", Включено");
                TrayButton(tray, admin + ", Выключено");
                TrayFocus(tray, startup);
                var expanded = Bounds(tray);
                Require(expanded.Height > compact.Height && expanded.Bottom == compact.Bottom
                    && area.Contains(expanded), "Showing the administrator switch broke the tray anchor");
                TrayKey(tray, startup, Keys.Down);
                TrayFocus(tray, admin);
                // The isolated consent provider declines the first request.
                TrayKey(tray, admin, Keys.Enter);
                TrayButton(tray, admin + ", Настраивается…");
                TrayButton(tray, admin + ", Выключено");
                TrayButton(tray, startup + ", Включено");
                TrayKey(tray, admin, Keys.Enter);
                TrayButton(tray, admin + ", Включено");
                TrayFocus(tray, admin);
                TrayKey(tray, admin, Keys.Escape);
                Until("Escape should close startup settings", () => !IsWindowVisible(tray));
                TrayOpen(tray, TrayContext, area);
                TrayButton(tray, admin + ", Включено");
                TrayClick(tray, admin);
                TrayButton(tray, admin + ", Выключено");
                TrayClick(tray, admin);
                TrayButton(tray, admin + ", Включено");
                TrayClick(tray, startup);
                TrayButton(tray, startup + ", Выключено");
                Require(Bounds(tray) == compact, "Disabling startup left the administrator row in the menu");
                TrayClick(tray, startup);
                TrayButton(tray, admin + ", Выключено");
                TrayClick(tray, startup);
                TrayButton(tray, startup + ", Выключено");
                TrayKey(tray, startup, Keys.Down);
                TrayFocus(tray, "Выйти из Rustile");
                TrayClick(tray, "Выйти из Rustile");
                Require(manager.Exited(5000) && manager.ExitCode == 0, "Isolated startup fixture did not exit cleanly");
            } finally {
                if (!manager.Exited(0)) {
                    int pid;
                    int thread = (int)GetWindowThreadProcessId(tray, out pid);
                    if (thread != 0 && pid == manager.Pid) {
                        PostThreadMessage(thread, 0x12 /* WM_QUIT */, IntPtr.Zero, IntPtr.Zero);
                        manager.Exited(5000);
                    }
                }
            }
        }
        Console.WriteLine("PASS  The administrator switch waits for approval, preserves regular startup after cancellation and restores the accepted mode on reopen (isolated registry and task)");
    }
    static void TraySettings(Child manager, string rustile, string desktop, List<Child> children,
        IntPtr[] pair, Rectangle[] originals, Rectangle area) {
        var tray = TrayWindow(manager);
        TrayOpen(tray, TrayKeyboard, area);
        TrayFocus(tray, "Мозаика");
        TrayKey(tray, "Мозаика", Keys.Down);
        TrayFocus(tray, "Горячие клавиши");
        TrayKey(tray, "Горячие клавиши", Keys.Escape);
        Until("Escape should dismiss the first popup after a hidden launch", () => !IsWindowVisible(tray));
        // Exercise a busy message queue as well: sent shell notifications take
        // precedence over posted messages, and WM_PAINT normally comes last.
        for (int i = 0; i < 2048; ++i) Message(tray, 0 /* WM_NULL */);
        TrayOpen(tray, TraySelect, area);
        TrayNotify(tray, TraySelect, area);
        Until("A second left click on the tray icon should close the popup", () => !IsWindowVisible(tray));
        TrayOpen(tray, TraySelect, area);
        TrayClickWithDeactivation(tray, TrayContext, area);
        TrayOpen(tray, TrayContext, area);
        TrayNotify(tray, TrayContext, area);
        Until("A second right click on the tray icon should close the popup", () => !IsWindowVisible(tray));
        TrayOpen(tray, TraySelect, area);
        TrayClickWithDeactivation(tray, TraySelect, area);
        TrayOpen(tray, TrayKeyboard, area);
        SendTrayMessage(tray, 0x0006 /* WM_ACTIVATE, WA_INACTIVE */, IntPtr.Zero, pair[0]);
        Until("Losing activation to another application should close the popup", () => !IsWindowVisible(tray));
        TrayOpen(tray, TraySelect, area);
        Console.WriteLine("PASS  Left, right and mixed tray activations toggle the flyout and reopen after outside dismissal");
        ClickArrow(tray, "Увеличить: Толщина обводки");
        TrayButton(tray, "Толщина обводки, 2 px");
        ClickArrow(tray, "Уменьшить: Толщина обводки");
        TrayButton(tray, "Толщина обводки, 1 px");
        TrayKey(tray, "Толщина обводки", Keys.Right);
        TrayButton(tray, "Толщина обводки, 2 px");
        TrayKey(tray, "Толщина обводки", Keys.Left);
        TrayButton(tray, "Толщина обводки, 1 px");

        EditNumber(tray, "Между окнами", "13", Keys.Enter);
        var gapRow = TrayButton(tray, "Между окнами, 13 px");
        // Wheel input may be delivered to the focused row even when the
        // pointer is over a different setting. Use actual screen coordinates.
        TrayKey(tray, "Между окнами", Keys.Up);
        TrayFocus(tray, "Толщина обводки");
        var elsewhere = TrayButton(tray, "Толщина обводки");
        Wheel(elsewhere, gapRow, 60);
        Wheel(elsewhere, gapRow, 60);
        TrayButton(tray, "Между окнами, 14 px");
        Wheel(elsewhere, gapRow, 360);
        TrayButton(tray, "Между окнами, 17 px");
        Wheel(elsewhere, gapRow, -120);
        TrayButton(tray, "Между окнами, 16 px");
        EditNumber(tray, "По краям экрана", "12", Keys.Enter);
        TrayButton(tray, "По краям экрана, 12 px");
        Until("The selected tray spacing should reach the DWM frames", () => HasSpacing(pair, area));
        EditNumber(tray, "Между окнами", "2", Keys.Escape);
        TrayButton(tray, "Между окнами, 16 px");
        var invalid = TypeNumber(tray, "Между окнами", "45");
        KeyDown(invalid, Keys.Enter);
        // After rejection the same edit is still active, with the draft selected.
        if (!PostMessage(invalid, 0x0102, (IntPtr)(int)'1', IntPtr.Zero)
            || !PostMessage(invalid, 0x0102, (IntPtr)(int)'5', IntPtr.Zero))
            throw new Win32Exception();
        KeyDown(invalid, Keys.Enter);
        TrayButton(tray, "Между окнами, 15 px");
        ClickArrow(tray, "Увеличить: Между окнами");
        TrayButton(tray, "Между окнами, 16 px");
        EditNumber(tray, "Толщина обводки", "4", Keys.Enter);
        TrayButton(tray, "Толщина обводки, 4 px");
        Require(!IsWindowEnabled(TrayButton(tray, "Увеличить: Толщина обводки")), "Maximum thickness should disable increase");
        ClickArrow(tray, "Уменьшить: Толщина обводки");
        TrayButton(tray, "Толщина обводки, 3 px");
        var beforeEditor = pair.Select(VisibleFrame).ToArray();
        var editor = OpenShortcutEditor(manager, tray, area);
        Require(GetWindow(editor, 4 /* GW_OWNER */) == IntPtr.Zero, "Shortcut editor should be an independent window");
        var initial = ShortcutLabels(editor);
        EmptyIdleHeader(editor);
        var viewHwnd = EditorControl(editor, 510);
        Require(initial.All(t => t.Length > 0) && GetDlgItem(viewHwnd, 1050) == IntPtr.Zero
            && initial[27].Contains("Win+") && initial[28].Contains("Win+") && initial[29].Contains("Win+"),
            "The page should expose all 50 editable shortcuts");
        Require(initial[34].EndsWith("Win+Колесо ↑") && initial[35].EndsWith("Win+Колесо ↓"),
            "The editor should show both desktop wheel bindings");
        for (int number = 1; number <= 10; number++)
            Require(initial[35 + number].EndsWith("Win+" + (number % 10)),
                "Wrong desktop number key for slot " + number);
        var arrows = new[] { "←", "→", "↑", "↓" };
        for (int i = 0; i < arrows.Length; ++i) {
            Require(initial[30 + i].EndsWith("Ctrl+Win+" + arrows[i]),
                "Desktop navigation should display its default shortcut for " + arrows[i]);
            Require(initial[46 + i].EndsWith("Ctrl+Win+Shift+" + arrows[i]),
                "Moving a window to another desktop should display its default shortcut for " + arrows[i]);
            Require(initial[19 + i].EndsWith("Win+Alt+" + arrows[i])
                && initial[23 + i].EndsWith("Win+Alt+Shift+" + arrows[i]),
                "Quick resize should display the default forward and reverse shortcuts for " + arrows[i]);
        }
        var view = Bounds(viewHwnd);
        if (area.Width * 96 >= 920 * GetDpiForWindow(editor) && area.Height * 96 >= 1000 * GetDpiForWindow(editor))
            Require(Enumerable.Range(1000, initial.Length).All(i => view.Contains(Bounds(EditorControl(editor, i)))),
                "Every shortcut should fit on the default-sized editor page");
        TrayOpen(tray, TraySelect, area);
        Require(OpenShortcutEditor(manager, tray, area) == editor, "Opening settings twice created another editor");
        var left = EditorControl(editor, 1004);
        var right = EditorControl(editor, 1005);
        var up = EditorControl(editor, 1006);
        var focusLeft = EditorControl(editor, 1000);
        var file = Path.Combine(Environment.GetEnvironmentVariable("LOCALAPPDATA"), "Rustile", "settings.toml");
        var beforeCapture = File.ReadAllText(file);
        var toggle = EditorControl(editor, 1029);
        BeginRecording(toggle);
        EnterChord(toggle, Keys.ControlKey, Keys.F8);
        Until("The mosaic shortcut should be reassignable", () => ControlText(toggle).EndsWith("Ctrl+F8"));
        ClickControl(editor, 1129);
        Until("Resetting the mosaic shortcut should restore its default", () =>
            ControlText(toggle) == initial[29] && File.ReadAllText(file) == beforeCapture);
        ClickControl(editor, 1005);
        Require(ControlText(right) == initial[5], "A single click started recording a shortcut");
        BeginRecording(left);
        EnterChord(left, Keys.LWin, Keys.Left); // Conflicts with Focus left.
        ChooseConflict(manager, editor, ConflictChoice.DefaultEnter);
        Require(ShortcutLabels(editor).SequenceEqual(initial) && File.ReadAllText(file) == beforeCapture,
            "Cancelling the conflict changed bindings");
        BeginRecording(focusLeft);
        EnterChord(focusLeft, Keys.LWin, Keys.Left); // same action's current binding
        Until("Recording the existing chord should finish normally", () => ControlText(focusLeft) == initial[0]);
        Require(File.ReadAllText(file) == beforeCapture, "A no-op capture changed the stored configuration");

        BeginRecording(up);
        CaptureKey(up, Keys.Menu, true, true);
        CaptureKey(up, Keys.Oemcomma, true, true);
        CaptureKey(up, Keys.Oemcomma, false, true);
        Until("The row should preview Alt plus the OEM key while Alt is held", () => ControlText(up).Contains("Alt+"));
        Require(File.ReadAllText(file) == beforeCapture, "The chord was saved before the last key was released");
        CaptureKey(up, Keys.Menu, false, true);
        Until("Releasing Alt must persist VK_OEM_COMMA", () =>
            System.Text.RegularExpressions.Regex.IsMatch(File.ReadAllText(file), @"\bkey\s*=\s*188\b")
            && ControlText(up).Contains("Alt+"));
        var punctuation = ControlText(up);
        BeginRecording(up);
        CaptureKey(up, Keys.Escape, true);
        CaptureKey(up, Keys.Escape, false);
        Until("Escape should cancel recording", () => ControlText(up) == punctuation);
        Require(IsWindowVisible(editor), "Cancelling recording closed the editor");
        BeginRecording(left);
        EnterChord(left, Keys.LWin, Keys.Left);
        ChooseConflict(manager, editor, ConflictChoice.TabEnter);
        Until("Reassignment must transfer the chord and unbind its former action", () =>
            ControlText(focusLeft).EndsWith("Не назначено") && ControlText(left).EndsWith("Win+←"));
        EmptyIdleHeader(editor);
        var oneOverride = File.ReadAllText(file);
        BeginRecording(right);
        EnterChord(right, Keys.ControlKey, Keys.F9);
        Until("Rebinding a second row should allow resetting it independently", () =>
            File.ReadAllText(file) != oneOverride && ControlText(right).Contains("Ctrl+F9"));
        ClickControl(editor, 1105);
        Until("Resetting Move right should recover its original binding", () => ControlText(right) == initial[5]);
        var expected = (string[])initial.Clone();
        expected[0] = ControlText(focusLeft);
        expected[4] = ControlText(left);
        expected[6] = punctuation;
        Require(ShortcutLabels(editor).SequenceEqual(expected), "A row reset changed a neighboring shortcut");
        Require(pair.Select(VisibleFrame).SequenceEqual(beforeEditor), "Shortcut editor changed the tiled layout");
        Message(editor, WmClose);
        Until("Closing the editor should leave Rustile running", () => !IsWindowVisible(editor) && !manager.Exited(0));
        TrayOpen(tray, TraySelect, area);
        editor = OpenShortcutEditor(manager, tray, area);
        Require(ShortcutLabels(editor).SequenceEqual(expected), "Reopening lost the transferred chord, disabled action or OEM key");
        EmptyIdleHeader(editor);
        Message(editor, WmClose);
        Until("Reopened editor should close", () => !IsWindowVisible(editor));
        Console.WriteLine("PASS  Inline tray controls stay open through clicks, arrow keys, fractional wheel input, typed values and cancellation");

        var tiled = pair.Select(VisibleFrame).ToArray();
        TrayChoose(tray, TrayContext, area, "Мозаика");
        TrayOpen(tray, TraySelect, area);
        TrayButton(tray, "Мозаика, Выключена");
        Require(pair.Select(VisibleFrame).SequenceEqual(tiled), "Pause moved a tiled window");
        var floating = tiled[0];
        floating.Offset(7, 5);
        SetFixtureFrame(pair[0], floating);
        Until("A paused tile should accept a manual move", () => VisibleFrame(pair[0]) == floating);
        // Exercise the shortcut hook's real message downstream of key matching.
        // A paused manager has no keyboard target, including on this private desktop.
        manager.Send(RustileShortcut, ToggleMosaic);
        TrayButton(tray, "Мозаика, Включена");
        Require(File.ReadAllLines(file).Contains("enabled = true"), "Shortcut resume was not persisted");
        Until("Shortcut resume should return the moved fixture to its tile", () => pair.Select(VisibleFrame).SequenceEqual(tiled));
        manager.Send(RustileShortcut, ToggleMosaic);
        TrayButton(tray, "Мозаика, Выключена");
        Require(File.ReadAllLines(file).Contains("enabled = false"), "Shortcut pause was not persisted");
        Require(pair.Select(VisibleFrame).SequenceEqual(tiled), "Shortcut pause changed existing window geometry");
        TrayKey(tray, "По краям экрана", Keys.End);
        TrayFocus(tray, "Выйти из Rustile");
        TrayClick(tray, "Выйти из Rustile");
        Require(manager.Exited(5000) && manager.ExitCode == 0, "Tray exit while paused failed");
        Until("Exit while paused should restore the original frames", () => pair.Select(Bounds).SequenceEqual(originals));

        var restarted = new Child(rustile, "", desktop, true);
        children.Add(restarted);
        tray = TrayWindow(restarted);
        TrayOpen(tray, TrayContext, area);
        TrayButton(tray, "Мозаика, Выключена");
        TrayButton(tray, "Толщина обводки, 3 px");
        Require(pair.Select(Bounds).SequenceEqual(originals), "Disabled startup tiled the windows");
        editor = OpenShortcutEditor(restarted, tray, area);
        var restored = ShortcutLabels(editor);
        Require(restored.SequenceEqual(expected), "Shortcut labels changed after restart: " +
            string.Join(", ", restored.Select((label, i) => label == expected[i] ? null :
                String.Format("row {0}: '{1}' -> '{2}'", i, expected[i], label)).Where(diff => diff != null)));
        oneOverride = File.ReadAllText(file);
        ClickControl(editor, 1100); // Focus left's factory chord now belongs to Move left.
        ChooseConflict(restarted, editor, ConflictChoice.Escape);
        Require(ShortcutLabels(editor).SequenceEqual(expected) && File.ReadAllText(file) == oneOverride,
            "Cancelling a conflicting reset silently took the other action's chord");
        ClickControl(editor, 1100);
        ChooseConflict(restarted, editor, ConflictChoice.ClickAssign);
        Until("Confirming the reset should return the factory chord and unbind the former owner", () =>
            ControlText(EditorControl(editor, 1000)) == initial[0]
            && ControlText(EditorControl(editor, 1004)).EndsWith("Не назначено"));
        oneOverride = File.ReadAllText(file);
        right = EditorControl(editor, 1005);
        BeginRecording(right);
        EnterChord(right, Keys.ControlKey, Keys.F9);
        Until("Move right should save Ctrl+F9", () => File.ReadAllText(file) != oneOverride
            && ControlText(right).Contains("Ctrl+F9"));
        ClickControl(editor, 505);
        Until("Reset all should restore every original chord", () => ShortcutLabels(editor).SequenceEqual(initial));
        Message(editor, WmClose);
        Until("The shortcut editor should close", () => !IsWindowVisible(editor));
        TrayOpen(tray, TraySelect, area);
        TrayClick(tray, "Мозаика");
        Until("Resume should use the stored inner and outer gaps", () => HasSpacing(pair, area));
        TrayOpen(tray, TraySelect, area);
        TrayClick(tray, "Выйти из Rustile");
        Require(restarted.Exited(5000) && restarted.ExitCode == 0, "Second tray exit failed");
        Until("Exit after resume should restore the original frames", () => pair.Select(Bounds).SequenceEqual(originals));
        var defaults = new Child(rustile, "", desktop, true);
        children.Add(defaults);
        tray = TrayWindow(defaults);
        TrayOpen(tray, TraySelect, area);
        editor = OpenShortcutEditor(defaults, tray, area);
        Require(ShortcutLabels(editor).SequenceEqual(initial), "Reset all did not survive restart");
        Message(editor, WmClose);
        Until("The final shortcut editor should close", () => !IsWindowVisible(editor));
        TrayChoose(tray, TraySelect, area, "Выйти из Rustile");
        Require(defaults.Exited(5000) && defaults.ExitCode == 0, "Tray exit after resetting shortcuts failed");
        Until("Exit after resetting shortcuts should restore fixture frames", () =>
            pair.Select(Bounds).SequenceEqual(originals));
        Console.WriteLine("PASS  Compact shortcut editor: OEM key on last release, conflict cancel/transfer, persisted disabled action and resets");
        Console.WriteLine("PASS  Tray pause, exit while paused, saved shortcut/outline/spacing, paused restart and resume");
        Console.WriteLine("PASS  The mosaic shortcut resumes without a target and pauses again, in sync with the open tray and saved state");
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
    static void BeginMouseDrag(IntPtr hwnd) {
        NotifyWinEvent(0x000a /* EVENT_SYSTEM_MOVESIZESTART */, hwnd, 0, 0);
        // WinEvent hooks are delivered asynchronously to the manager thread.
        // Give it the same stationary start frame as a real border press.
        Thread.Sleep(120);
    }
    static void SetDragFrame(IntPtr hwnd, Rectangle frame) {
        SetFixtureFrame(hwnd, frame);
        NotifyWinEvent(0x800b /* EVENT_OBJECT_LOCATIONCHANGE */, hwnd, 0, 0);
    }
    static void EndMouseDrag(IntPtr hwnd) {
        NotifyWinEvent(0x000b /* EVENT_SYSTEM_MOVESIZEEND */, hwnd, 0, 0);
    }
    static Point Center(Rectangle frame) {
        return new Point(frame.Left + frame.Width / 2, frame.Top + frame.Height / 2);
    }
    static void MoveFrameTo(IntPtr hwnd, Rectangle source, Point center) {
        // A private desktop cannot read the input cursor. Windows still reports
        // the HWND translation, so its center supplies the drag point here.
        source.Offset(center.X - Center(source).X, center.Y - Center(source).Y);
        SetDragFrame(hwnd, source);
    }
    static Rectangle WaitForDropPreview(Child manager, string description, Func<Rectangle, bool> expected) {
        var preview = Rectangle.Empty;
        Until(description, () => {
            var hwnd = ManagerWindow(manager, "Rustile.DropPreview", true);
            if (hwnd == IntPtr.Zero) return false;
            preview = Bounds(hwnd);
            if (!expected(preview)) return false;
            const int clickThrough = 0x00000020 /* WS_EX_TRANSPARENT */ | 0x08000000 /* WS_EX_NOACTIVATE */;
            Require((GetWindowLongW(hwnd, -20 /* GWL_EXSTYLE */) & clickThrough) == clickThrough,
                "The visible drop preview must pass through input and avoid activation");
            var center = Center(preview);
            var point = (IntPtr)((center.X & 0xffff) | ((center.Y & 0xffff) << 16));
            UIntPtr result;
            Require(SendMessageTimeout(hwnd, 0x84 /* WM_NCHITTEST */, IntPtr.Zero, point,
                2 /* SMTO_ABORTIFHUNG */, 500, out result) != IntPtr.Zero
                && unchecked((int)result.ToUInt64()) == -1 /* HTTRANSPARENT */,
                "The visible drop preview intercepted a hit on its surface");
            return true;
        });
        return preview;
    }
    static void MouseMove(Child manager, IntPtr[] windows, int anchor, int gap, Rectangle work) {
        var original = windows.Select(VisibleFrame).ToArray();
        int target = 1 - anchor;
        var active = windows[2];
        var source = original[2];
        var host = original[target];
        var center = Center(host);
        bool stack = host.Top == original.Aggregate(Rectangle.Union).Top
            && host.Bottom == original.Aggregate(Rectangle.Union).Bottom;
        var edge = stack ? new Point(center.X, host.Top + 3) : new Point(host.Left + 3, center.Y);
        var foreground = GetForegroundWindow();
        Func<bool> hidden = () => ManagerWindow(manager, "Rustile.DropPreview", true) == IntPtr.Zero;
        Func<bool> unchanged = () => windows.Select(VisibleFrame).SequenceEqual(original);
        Func<Rectangle, bool> firstHalf = r => stack
            ? r.Left == host.Left && r.Right == host.Right && r.Top == host.Top
                && Math.Abs(2 * r.Height + gap - host.Height) <= 1
            : r.Top == host.Top && r.Bottom == host.Bottom && r.Left == host.Left
                && Math.Abs(2 * r.Width + gap - host.Width) <= 1;

        BeginMouseDrag(active);
        MoveFrameTo(active, source, edge);
        WaitForDropPreview(manager, "A native caption drag to the target edge should show a half-tile preview", firstHalf);
        MoveFrameTo(active, source, center);
        var centerPreview = WaitForDropPreview(manager, "Retargeting the center must show the whole target tile", r => r == host);
        Require(VisibleFrame(windows[target]) == host && VisibleFrame(windows[anchor]) == original[anchor],
            "Showing a drop preview moved tiles before release");
        Require(GetForegroundWindow() == foreground, "A drop preview took focus during the drag");
        EndMouseDrag(active);
        Until("Releasing at the center must exchange exactly the two slots from the preview", () =>
            hidden() && VisibleFrame(active) == centerPreview && VisibleFrame(windows[target]) == source
            && VisibleFrame(windows[anchor]) == original[anchor]);

        BeginMouseDrag(active);
        MoveFrameTo(active, centerPreview, Center(source));
        WaitForDropPreview(manager, "The center of the original slot should accept the return", r => r == source);
        EndMouseDrag(active);
        Until("A second center drop should restore the original layout", () => hidden() && unchanged());

        BeginMouseDrag(active);
        MoveFrameTo(active, source, center);
        WaitForDropPreview(manager, "The target should be visible before cancelling", r => r == host);
        // Native Esc restores the dragged HWND to its start frame before END.
        SetDragFrame(active, source);
        EndMouseDrag(active);
        Until("A cancelled caption drag must dismiss its preview and preserve all frames", () => hidden() && unchanged());

        BeginMouseDrag(active);
        MoveFrameTo(active, source, center);
        WaitForDropPreview(manager, "A fresh drag should show the target again", r => r == host);
        MoveFrameTo(active, source, new Point(work.Right + 16, center.Y));
        Until("Leaving the work area should clear the drop preview before END", hidden);
        EndMouseDrag(active);
        Until("Dropping outside the work area should restore all original slots", () => hidden() && unchanged());

        BeginMouseDrag(active);
        MoveFrameTo(active, source, edge);
        var split = WaitForDropPreview(manager, "Returning to the edge should show its split", firstHalf);
        EndMouseDrag(active);
        var remainder = stack ? Rectangle.FromLTRB(host.Left, split.Bottom + gap, host.Right, host.Bottom)
            : Rectangle.FromLTRB(split.Right + gap, host.Top, host.Right, host.Bottom);
        Until("The released tile should occupy its exact preview, the target should fill the rest, and its old sibling should expand", () =>
            hidden() && VisibleFrame(active) == split && VisibleFrame(windows[target]) == remainder
            && VisibleFrame(windows[anchor]) == Rectangle.Union(source, original[anchor]));
        Require(GetForegroundWindow() == foreground, "Completing the drag changed the foreground window");
        Console.WriteLine("PASS  Native caption drag previews edge and center without focus changes, commits the exact shown tile, and cancels on Escape or outside the work area");
    }
    static void MouseResize(Child manager, IntPtr[] windows, int anchor, int gap, Rectangle work) {
        var original = windows.Select(VisibleFrame).ToArray();
        var active = windows[anchor];
        var source = original[anchor];
        int sibling = NeighborDirection(source, original[2]);
        int other = NeighborDirection(source, original[1 - anchor]);
        Require(sibling >= 0 && other >= 0 && sibling / 2 != other / 2,
            "Expected two perpendicular shared borders on the original focused tile");
        bool left = sibling == 0 || other == 0, top = sibling == 2 || other == 2;
        int x = left ? source.Left - gap : source.Right;
        int y = top ? source.Top - gap : source.Bottom;
        int signX = left ? -1 : 1, signY = top ? -1 : 1;
        // Move only edges bordering the shared gaps. The monitor perimeter and
        // all unrelated boundaries retain their exact original DWM coordinates.
        Func<int, int, Rectangle[]> expected = (dx, dy) => original.Select(r => Rectangle.FromLTRB(
            r.Left == x + gap ? r.Left + dx : r.Left, r.Top == y + gap ? r.Top + dy : r.Top,
            r.Right == x ? r.Right + dx : r.Right, r.Bottom == y ? r.Bottom + dy : r.Bottom)).ToArray();
        Func<Rectangle[], bool> matches = rectangles => windows.Select(VisibleFrame).SequenceEqual(rectangles);
        var foreground = GetForegroundWindow();

        BeginMouseDrag(active);
        var grown = expected(signX * 32, signY * 24);
        SetDragFrame(active, grown[anchor]);
        Until("Both neighbors should follow a growing corner before MOVESIZEEND", () => matches(grown));
        var smaller = expected(-signX * 29, -signY * 19);
        SetDragFrame(active, smaller[anchor]);
        Until("Both neighbors should follow the same corner back past its start", () => matches(smaller));
        EndMouseDrag(active);
        Thread.Sleep(200);
        SendTrayMessage(TrayWindow(manager), 0x1a /* WM_SETTINGCHANGE */, (IntPtr)47 /* SPI_SETWORKAREA */, IntPtr.Zero);
        Thread.Sleep(350);
        Require(matches(smaller), "The resized corner must survive release and desktop reconciliation");

        BeginMouseDrag(active);
        var translated = smaller[anchor];
        translated.Offset(13, 9);
        SetDragFrame(active, translated);
        Thread.Sleep(150);
        Require(VisibleFrame(windows[2]) == smaller[2] && VisibleFrame(windows[1 - anchor]) == smaller[1 - anchor],
            "Moving the whole frame must not resize its neighbors");
        EndMouseDrag(active);
        Until("Releasing a translated tile should restore its saved slot", () => matches(smaller));

        // Attempt to push one sibling below the minimum it advertised through
        // WM_GETMINMAXINFO. The native tracking minimum is an outer dimension.
        bool horizontal = sibling < 2;
        var minOuter = DragPeerMinimum(work);
        var peerOuter = Bounds(windows[2]);
        int extent = horizontal ? original[2].Width : original[2].Height;
        int min = horizontal ? Math.Max(160, minOuter.Width - (peerOuter.Width - smaller[2].Width))
            : Math.Max(100, minOuter.Height - (peerOuter.Height - smaller[2].Height));
        Func<int, Rectangle[]> shrinkPeerTo = size => horizontal
            ? expected(signX * (extent - size), -signY * 19)
            : expected(-signX * 29, signY * (extent - size));
        Require(min < extent && min > 24, "Test setup must permit resizing the sibling into its native minimum");
        var limit = shrinkPeerTo(min);
        BeginMouseDrag(active);
        SetDragFrame(active, shrinkPeerTo(24)[anchor]);
        Until("Dragging past the sibling's native minimum should clamp its live frame", () => VisibleFrame(windows[2]) == limit[2]);
        EndMouseDrag(active);
        Until("At release, the tile and its neighbor must meet at their clamped gap", () => matches(limit));

        BeginMouseDrag(active);
        SetDragFrame(active, smaller[anchor]);
        Until("A new preview should start from the committed limit", () => matches(smaller));
        // A native cancellation restores the HWND before the end notification.
        SetDragFrame(active, limit[anchor]);
        EndMouseDrag(active);
        Until("Cancelling a corner resize must restore both neighbors", () => matches(limit));

        // A quick drag may have already moved the HWND when START is delivered.
        NotifyWinEvent(0x000a /* EVENT_SYSTEM_MOVESIZESTART */, active, 0, 0);
        SetDragFrame(active, original[anchor]);
        EndMouseDrag(active);
        Until("A quick drag back should restore both original borders", () => matches(original));
        Thread.Sleep(200);
        Require(matches(original) && GetForegroundWindow() == foreground, "Restoring the layout changed the tiles or focus");
        Console.WriteLine("PASS  Native mouse drag resizes both shared edges live, clamps at the peer minimum, persists on release, and ignores pure moves");
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
        int index = Array.IndexOf(handles, active);
        var initialFrame = VisibleFrame(active);
        var work = handles.Select(VisibleFrame).Aggregate(Rectangle.Union);
        foreach (int direction in new[] { 20, 21, 22, 23 }) {
            if ((direction == 20 && initialFrame.Left == work.Left)
                || (direction == 21 && initialFrame.Right == work.Right)
                || (direction == 22 && initialFrame.Top == work.Top)
                || (direction == 23 && initialFrame.Bottom == work.Bottom)) continue;
            manager.Send(RustileShortcut, direction, active);
            Thread.Sleep(60);
            if (!handles.Select(Bounds).SequenceEqual(before)) { moved = direction; break; }
        }
        Require(moved >= 0, "Resize command did not move a divider");
        bool horizontal = moved < 22;
        Func<Rectangle, int> extent = r => horizontal ? r.Width : r.Height;
        var expanded = handles.Select(Bounds).ToArray();
        Require(extent(expanded[index]) > extent(before[index]), "Plain arrow toward a neighbor should grow the active tile");
        manager.Send(RustileShortcut, 30 + (moved - 20), active);
        Until("Shift with the same arrow should restore every native frame exactly",
            () => handles.Select(Bounds).SequenceEqual(before));
        manager.Send(RustileShortcut, moved, active);
        Until("Repeating the plain arrow should grow the same edge again", () => handles.Select(Bounds).SequenceEqual(expanded));
        manager.Send(RustileShortcut, 41, active);
        Func<bool> noHint = () => ManagerWindow(manager, "Rustile.ResizeHint", true) == IntPtr.Zero;
        Until("Escape should dismiss the hint and keep the displayed split",
            () => noHint() && handles.Select(Bounds).SequenceEqual(expanded));
        manager.Send(RustileShortcut, 70 + moved - 20, active);
        Until("Reverse quick resize should work immediately after Escape",
            () => noHint() && extent(Bounds(active)) < extent(expanded[index]));
        manager.Send(RustileShortcut, 60 + moved - 20, active);
        Until("Forward quick resize should change the same divider without a modal hint",
            () => noHint() && handles.Select(Bounds).SequenceEqual(expanded));
        manager.Send(RustileShortcut, 1, active);
        Until("The modal hint should appear again", () => !noHint());
        manager.Send(RustileShortcut, 30 + moved - 20, active);
        Until("The inverse modal command should update the same divider", () => extent(Bounds(active)) < extent(expanded[index]));
        var latest = Bounds(active);
        manager.Send(RustileShortcut, 40, active);
        Until("Enter should dismiss the hint and preserve the active window's latest size",
            () => noHint() && Bounds(active) == latest);
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
            Environment.SetEnvironmentVariable("RUSTILE_SMOKE_DESKTOP", desktopName);
            using (var keys = new Child(managerTests, "--exact runtime::tests::isolated_shortcut_ownership --ignored", desktopName)) {
                Require(keys.Exited(10000) && keys.ExitCode == 0, "Shortcut callback ownership test failed on the empty private desktop");
            }
            Console.WriteLine("PASS  Shortcut callback consumes assigned presses, repeats and releases with no foreground or failed delivery; unassigned keys pass");
            var self = Process.GetCurrentProcess().MainModule.FileName;
            var windows = new IntPtr[3];
            var originals = new Rectangle[3];
            var fixtures = new Child[3];
            Action<int, bool> launchFixture = delegate(int index, bool dragPeer) {
                var fixture = new Child(self, "fixture " + index + (dragPeer ? " drag-peer" : ""), desktopName);
                children.Add(fixture);
                fixtures[index] = fixture;
                Until("Fixture should create its window", () => (windows[index] = Find(fixture.Pid, false)) != IntPtr.Zero);
                originals[index] = Bounds(windows[index]);
            };
            Action<int> launch = index => launchFixture(index, false);
            launch(0); launch(1);
            FullscreenRoundTrips(rustile, desktopName, children, windows[0], windows[1], originals);
            // Exercise the default Windows modifier even when another application
            // has already registered Win+Enter on this desktop.
            ownsShortcut = RegisterHotKey(IntPtr.Zero, fixtureShortcut, 8, 13);
            if (!ownsShortcut)
                Require(Marshal.GetLastWin32Error() == 1409, "Could not establish a Win+Enter conflict");
            bool quitWasAvailable = QuitChordAvailable();
            var manager = new Child(rustile, "", desktopName, true);
            children.Add(manager);
            bool elevated = manager.IsElevated();
            Require(fixtures.Take(2).All(fixture => fixture.IsElevated() == elevated),
                "This scenario requires both windows to run at the same elevation as Rustile");
            var area = Screen.PrimaryScreen.WorkingArea;
            Until("Existing windows should tile on launch", () => {
                Require(!manager.Exited(0), "Rustile stopped before the initial layout");
                return Tiled(area, windows.Take(2).ToArray());
            });
            Console.WriteLine("PASS  Default Windows-key profile starts and tiles with Win+Enter already registered");
            Console.WriteLine("PASS  Rustile tiles two same-elevation processes ({0})", elevated ? "elevated" : "standard");
            bool quitAvailable = QuitChordAvailable();
            Require(!quitWasAvailable || quitAvailable, "Rustile claimed Ctrl+Alt+Shift+Q on startup");
            Console.WriteLine(quitAvailable ? "PASS  The former global quit chord stays available"
                : "SKIP  Another process had already registered the former quit chord before startup");
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
            launchFixture(2, true);
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
            MouseResize(manager, windows, anchor, outerGap, area);
            MouseMove(manager, windows, anchor, outerGap, area);

            bool resized = ResizeOnHiddenDesktop(manager, windows);
            Console.WriteLine(resized ? "PASS  Modal and quick resize share a divider; Escape and Enter preserve the latest frames"
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
            StartupSettings(managerTests, desktopName, area);
            Environment.SetEnvironmentVariable("RUSTILE_SMOKE_MIN_HEIGHT", TallMinimum(area).ToString());
            Environment.SetEnvironmentVariable("RUSTILE_SMOKE_WINDOWS", string.Join(" ", windows.Select(w => w.ToInt64())));
            // The hidden process has no console. Surface a failing Rust geometry
            // assertion in the smoke output, including actual and expected frames.
            string failureFile = Path.ChangeExtension(self, ".log");
            Environment.SetEnvironmentVariable("RUSTILE_SMOKE_FAILURE", failureFile);
            Action<string> geometryTest = delegate(string name) {
                try {
                    var geometry = new Child(managerTests, "--exact manager::desktop_test::" + name + " --ignored", desktopName);
                    children.Add(geometry);
                    Require(geometry.Exited(20000), "Hidden geometry test timed out: " + name);
                    Require(geometry.ExitCode == 0, File.Exists(failureFile)
                        ? File.ReadAllText(failureFile) : "Hidden geometry test failed: " + name);
                } finally {
                    File.Delete(failureFile);
                }
            };
            geometryTest("move_windows");
            Require(GetProp(windows[0], "Rustile.Smoke.MovePassed") != IntPtr.Zero, "Manager test did not run");
            Until("Manager should restore all six fixtures", () => windows.Select(Bounds).SequenceEqual(originals));
            Console.WriteLine("PASS  Five real windows: three plus two slots, level DWM heights after resize and horizontal moves");
            Console.WriteLine("PASS  Quick resize and its inverse preserve the off-grid divider with no modal hint; shell focus commits the current resize");
            Console.WriteLine("PASS  Virtual desktop switch events preserve both layouts, resized splits and pending moves on the first return");
            Console.WriteLine("PASS  A borderless fullscreen window can transfer and receive focus on the destination, keeping its style and bounds with tiling on and off");
            Console.WriteLine("PASS  A hidden, app-cloaked tray window releases the bottom tile and rejoins when shown");
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
            Array.Resize(ref windows, 7);
            Array.Resize(ref originals, 7);
            Array.Resize(ref fixtures, 7);
            launch(6);
            Environment.SetEnvironmentVariable("RUSTILE_SMOKE_WINDOWS", string.Join(" ", windows.Select(w => w.ToInt64())));
            geometryTest("share_offset_upper_tile");
            Require(GetProp(windows[4], "Rustile.Smoke.OffsetSevenPassed") != IntPtr.Zero, "Seven-window geometry test did not run");
            Until("Manager should restore all seven fixtures", () => windows.Select(Bounds).SequenceEqual(originals));
            Console.WriteLine("PASS  Seven windows: Up into an offset wider tile shares that tile and fills the bottom row; a second Up exchanges the original slots");
            SetMode(windows[4], FixtureMode.Borderless);
            SetMode(windows[5], FixtureMode.Borderless);
            Environment.SetEnvironmentVariable("RUSTILE_SMOKE_WINDOWS", string.Join(" ", windows.Take(6).Select(w => w.ToInt64())));
            geometryTest("window_controls");
            Require(fixtures.Skip(2).Take(4).All(fixture => fixture.Exited(3000)),
                "The tiled, floating and fullscreen fixtures should exit after accepting close");
            Until("The two surviving fixtures should return to their original bounds", () =>
                windows.Take(2).Select(Bounds).SequenceEqual(originals.Take(2)));
            Console.WriteLine("PASS  Win+F restores custom tile sizes; Win+Q honors a veto and reclaims the tile after close");
            Console.WriteLine("PASS  Fullscreen Win+Q supports a close veto and an accepted exit with mosaic on and off; Win+F preserves fullscreen");
            Console.WriteLine("PASS  With mosaic paused at startup, Win+F restores floating bounds and Win+Q closes the active window, preserving its peer");
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
            if ((args.Length == 2 || args.Length == 3) && args[0] == "fixture")
                Application.Run(new Fixture(int.Parse(args[1]), args.Length == 3 && args[2] == "drag-peer"));
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
