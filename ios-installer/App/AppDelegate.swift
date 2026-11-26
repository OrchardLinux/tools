// Orchard Installer: installs the system from inside iOS. UIKit, for iOS 12
// and later; the screens are installer-app.pen's frames I0.1 to I3.8.
import UIKit

private func makeWindow(_ window: UIWindow) -> UIWindow {
    if #available(iOS 13, *) { window.overrideUserInterfaceStyle = .dark }
    window.backgroundColor = Theme.background
    let mock = Mock.frame
    if mock != nil, let size = Mock.size { window.frame = CGRect(origin: .zero, size: size) }
    window.rootViewController = RootViewController(mock: mock)
    window.makeKeyAndVisible()
    return window
}

/// iOS 13 and later: the window comes from the scene (current iOS requires it).
@available(iOS 13, *)
@objc(SceneDelegate)
final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        window = makeWindow(UIWindow(windowScene: scene))
    }
}

@UIApplicationMain
final class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        // iOS 12 has no scenes: the window is the app's own.
        if #available(iOS 13, *) {} else { window = makeWindow(UIWindow(frame: UIScreen.main.bounds)) }
        return true
    }
}
