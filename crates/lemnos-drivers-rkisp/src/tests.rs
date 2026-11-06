use crate::Isp;

#[test]
fn the_soc_names_its_isp() {
    let root = |s: &[&'static str]| Isp::from_compatibles(s.iter().copied());
    assert_eq!(root(&["radxa,zero3", "rockchip,rk3566"]), Some(Isp::Isp21));
    assert_eq!(
        root(&["rockchip,rk3568-evb", "rockchip,rk3568"]),
        Some(Isp::Isp21)
    );
    assert_eq!(
        root(&["radxa,rock-5a", "rockchip,rk3588s"]),
        Some(Isp::Isp30)
    );
    assert_eq!(root(&["luckfox,pico", "rockchip,rv1106"]), Some(Isp::Isp32));
    assert_eq!(
        root(&["rockchip,rk3562-evb", "rockchip,rk3562"]),
        Some(Isp::Isp32Lite)
    );
    assert_eq!(root(&["rockchip,rv1103b"]), None);
    assert_eq!(root(&["rockchip,rk3576"]), None);
    assert_eq!(
        root(&["raspberrypi,5-compute-module", "brcm,bcm2712"]),
        None
    );
}
