trait WindowControl {
    pub fn minimize();
    pub fn maximize();
    pub fn fullscreen();

    pub fn unminimize();
    pub fn unmaximize();
    pub fn unfullscreen();

    pub fn activate();
    pub fn close();
}
