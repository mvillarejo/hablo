const MARK_SRC = "/Images/hablo-mark.svg";

export function AppLogo() {
  return (
    <>
      <img src={MARK_SRC} alt="" className="logo-img" draggable={false} />
      <span className="logo-wordmark">Hablo</span>
    </>
  );
}
