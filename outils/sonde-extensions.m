// Sonde des extensions sur macOS (outil de diagnostic, hors de Feather).
// Charge une extension décompressée avec WKWebExtensionController, comme
// Feather, ouvre sa fenêtre (bouton) et sa page d'options, puis écrit ce que
// WebKit en dit : erreurs, contenu des pages, captures d'écran.
// Compilation : clang -fobjc-arc -framework Cocoa -framework WebKit sonde-extensions.m -o sonde
// Usage : ./sonde <dossier de l'extension> <nom> <dossier de sortie>
#import <Cocoa/Cocoa.h>
#import <WebKit/WebKit.h>

static void say(NSString *fmt, ...) {
    va_list args;
    va_start(args, fmt);
    NSString *s = [[NSString alloc] initWithFormat:fmt arguments:args];
    va_end(args);
    printf("%s\n", s.UTF8String);
    fflush(stdout);
}

static NSString *outDir;
static NSString *name;

// Erreurs JavaScript des pages de l'extension (fenêtre, options).
static NSString *const kCatch =
    @"window.__e = [];"
     "addEventListener('error', (e) => __e.push('error ' + e.message + ' @' + e.filename + ':' + e.lineno), true);"
     "addEventListener('unhandledrejection', (e) => __e.push('rejet ' + String(e.reason && (e.reason.stack || e.reason.message) || e.reason)));"
     "(() => { const o = console.error; console.error = (...a) => { __e.push('console ' + a.map(String).join(' ')); o.apply(console, a); }; })();";

static NSString *const kProbe =
    @"JSON.stringify({href: location.href, ready: document.readyState,"
     " html: document.documentElement ? document.documentElement.outerHTML.length : -1,"
     " text: (document.body && document.body.innerText || '').slice(0, 400),"
     " w: document.documentElement.scrollWidth, h: document.documentElement.scrollHeight,"
     " chrome: typeof chrome, browser: typeof browser,"
     " runtime: typeof (window.chrome && chrome.runtime && chrome.runtime.sendMessage),"
     " body: (document.body && document.body.innerHTML || '').slice(0, 600),"
     " errs: (window.__e || []).slice(0, 30)})";

// Ce que l'arrière-plan a noté (voir l'enveloppe ajoutée par le flux de travail).
static NSString *const kStorage =
    @"const api = (typeof browser !== 'undefined' ? browser : chrome);"
     "const all = await api.storage.local.get(null);"
     "return JSON.stringify({bgerr: all.__bgerr || null, keys: Object.keys(all).slice(0, 20)});";

// L'arrière-plan répond-il ? Et l'onglet actif est-il visible depuis la page ?
static NSString *const kMessages =
    @"const api = (typeof browser !== 'undefined' ? browser : chrome);"
     "const wait = (p) => Promise.race([p.then((r) => 'ok ' + JSON.stringify(r).slice(0, 300), (e) => 'erreur ' + e), new Promise((r) => setTimeout(() => r('rien en 4 s'), 4000))]);"
     "const out = {};"
     "out.tabs = await wait(api.tabs.query({active: true, currentWindow: true}).then((t) => t.map((x) => x.id + ' ' + x.url)));"
     "out.win = await wait(api.windows.getCurrent());"
     "out.message = await wait(api.runtime.sendMessage({what: 'sonde'}));"
     "return JSON.stringify(out);";

// Vue cachée de l'arrière-plan (interface privée de WebKit, seulement pour la sonde).
static WKWebView *backgroundView(WKWebExtensionContext *c) {
    @try { return [c valueForKey:@"_backgroundWebView"]; } @catch (NSException *e) { return nil; }
}

static void probeBackground(WKWebExtensionContext *c, NSString *when) {
    WKWebView *bg = backgroundView(c);
    say(@"arrière-plan (%@) : vue %@, adresse %@, chargement %d", when, bg, bg.URL, bg.loading);
    if (!bg) return;
    [bg evaluateJavaScript:@"JSON.stringify({href: location.href, ready: document.readyState, scripts: [...document.scripts].map((s) => s.src + ' ' + s.type), sw: navigator.serviceWorker ? 'oui' : 'non', errs: (window.__e || []).slice(0, 30), text: (document.body && document.body.innerText || '').slice(0, 300)})"
         completionHandler:^(id r, NSError *e) { say(@"arrière-plan (%@) : %@%@", when, r ?: @"", e ? e.userInfo : @""); }];
}

@class Win;
@interface Tab : NSObject <WKWebExtensionTab>
@property(strong) WKWebView *web;
@property(weak) Win *win;
@end

@interface Win : NSObject <WKWebExtensionWindow>
@property(strong) Tab *tab;
@end

@implementation Tab
- (id<WKWebExtensionWindow>)windowForWebExtensionContext:(WKWebExtensionContext *)c { return self.win; }
- (WKWebView *)webViewForWebExtensionContext:(WKWebExtensionContext *)c { return self.web; }
- (NSURL *)urlForWebExtensionContext:(WKWebExtensionContext *)c { return self.web.URL; }
- (NSString *)titleForWebExtensionContext:(WKWebExtensionContext *)c { return self.web.title; }
- (BOOL)isSelectedForWebExtensionContext:(WKWebExtensionContext *)c { return YES; }
- (BOOL)isLoadingCompleteForWebExtensionContext:(WKWebExtensionContext *)c { return !self.web.loading; }
- (BOOL)shouldGrantPermissionsOnUserGestureForWebExtensionContext:(WKWebExtensionContext *)c { return YES; }
@end

@implementation Win
- (NSArray<id<WKWebExtensionTab>> *)tabsForWebExtensionContext:(WKWebExtensionContext *)c { return @[ self.tab ]; }
- (id<WKWebExtensionTab>)activeTabForWebExtensionContext:(WKWebExtensionContext *)c { return self.tab; }
- (WKWebExtensionWindowType)windowTypeForWebExtensionContext:(WKWebExtensionContext *)c { return WKWebExtensionWindowTypeNormal; }
- (WKWebExtensionWindowState)windowStateForWebExtensionContext:(WKWebExtensionContext *)c { return WKWebExtensionWindowStateNormal; }
- (BOOL)isPrivateForWebExtensionContext:(WKWebExtensionContext *)c { return NO; }
@end

@interface Del : NSObject <WKWebExtensionControllerDelegate>
@property(strong) Win *win;
@property(strong) WKWebExtensionAction *presented;
@end

@implementation Del
- (NSArray<id<WKWebExtensionWindow>> *)webExtensionController:(WKWebExtensionController *)c openWindowsForExtensionContext:(WKWebExtensionContext *)x { return @[ self.win ]; }
- (id<WKWebExtensionWindow>)webExtensionController:(WKWebExtensionController *)c focusedWindowForExtensionContext:(WKWebExtensionContext *)x { return self.win; }
- (void)webExtensionController:(WKWebExtensionController *)c presentPopupForAction:(WKWebExtensionAction *)action forExtensionContext:(WKWebExtensionContext *)x completionHandler:(void (^)(NSError *))done {
    say(@"[délégué] présenter la fenêtre (popupWebView=%@)", action.popupWebView.URL);
    self.presented = action;
    done(nil);
}
- (void)webExtensionController:(WKWebExtensionController *)c promptForPermissions:(NSSet<WKWebExtensionPermission> *)p inTab:(id<WKWebExtensionTab>)t forExtensionContext:(WKWebExtensionContext *)x completionHandler:(void (^)(NSSet<WKWebExtensionPermission> *, NSDate *))done {
    say(@"[délégué] autorisations demandées : %@", p);
    done(p, nil);
}
- (void)webExtensionController:(WKWebExtensionController *)c promptForPermissionMatchPatterns:(NSSet<WKWebExtensionMatchPattern *> *)p inTab:(id<WKWebExtensionTab>)t forExtensionContext:(WKWebExtensionContext *)x completionHandler:(void (^)(NSSet<WKWebExtensionMatchPattern *> *, NSDate *))done {
    say(@"[délégué] sites demandés : %@", p);
    done(p, nil);
}
- (void)webExtensionController:(WKWebExtensionController *)c promptForPermissionToAccessURLs:(NSSet<NSURL *> *)p inTab:(id<WKWebExtensionTab>)t forExtensionContext:(WKWebExtensionContext *)x completionHandler:(void (^)(NSSet<NSURL *> *, NSDate *))done {
    say(@"[délégué] adresses demandées : %@", p);
    done(p, nil);
}
- (void)webExtensionController:(WKWebExtensionController *)c openNewTabUsingConfiguration:(WKWebExtensionTabConfiguration *)cfg forExtensionContext:(WKWebExtensionContext *)x completionHandler:(void (^)(id<WKWebExtensionTab>, NSError *))done {
    say(@"[délégué] nouvel onglet demandé : %@", cfg.url);
    done(nil, nil);
}
- (void)webExtensionController:(WKWebExtensionController *)c openOptionsPageForExtensionContext:(WKWebExtensionContext *)x completionHandler:(void (^)(NSError *))done {
    say(@"[délégué] page d'options demandée");
    done(nil);
}
@end

static void snapshot(WKWebView *web, NSString *file) {
    [web takeSnapshotWithConfiguration:nil completionHandler:^(NSImage *image, NSError *error) {
        if (!image) { say(@"capture %@ impossible : %@", file, error); return; }
        CGImageRef cg = [image CGImageForProposedRect:NULL context:nil hints:nil];
        NSBitmapImageRep *rep = [[NSBitmapImageRep alloc] initWithCGImage:cg];
        NSData *png = [rep representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
        NSString *path = [outDir stringByAppendingPathComponent:[NSString stringWithFormat:@"%@-%@.png", name, file]];
        [png writeToFile:path atomically:YES];
        say(@"capture : %@ (%.0fx%.0f)", path, image.size.width, image.size.height);
    }];
}

static void probe(WKWebView *web, NSString *what) {
    say(@"%@ : vue %@, cadre %@", what, web.URL, NSStringFromRect(web.frame));
    [web evaluateJavaScript:kProbe completionHandler:^(id result, NSError *error) {
        say(@"%@ : %@%@", what, result ?: @"", error ? error.localizedDescription : @"");
    }];
    if ([web.URL.scheme isEqualToString:@"webkit-extension"])
        [web callAsyncJavaScript:kStorage arguments:nil inFrame:nil inContentWorld:WKContentWorld.pageWorld completionHandler:^(id result, NSError *error) {
            say(@"%@ stockage : %@%@", what, result ?: @"", error ? error.userInfo : @"");
        }];
    if ([web.URL.scheme isEqualToString:@"webkit-extension"])
        [web callAsyncJavaScript:kMessages arguments:nil inFrame:nil inContentWorld:WKContentWorld.pageWorld completionHandler:^(id result, NSError *error) {
            say(@"%@ messages : %@%@", what, result ?: @"", error ? error.userInfo : @"");
        }];
    snapshot(web, what);
}

static void after(double seconds, dispatch_block_t block) {
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(seconds * NSEC_PER_SEC)), dispatch_get_main_queue(), block);
}

static WKWebExtensionController *controller;
static WKWebExtensionContext *context;
static Del *delegate;
static Tab *tab;
static Win *win;
static NSWindow *window;
static NSWindow *optionsWindow;
static WKWebView *optionsView;
static WKWebView *logView;
static NSURL *dirURL;

static void run(WKWebExtension *ext) {
    say(@"extension : %@ %@ (manifeste v%.0f)", ext.displayName, ext.version, ext.manifestVersion);
    say(@"erreurs de l'extension : %@", ext.errors);
    say(@"autorisations : %@", ext.requestedPermissions);
    say(@"autorisations optionnelles : %@", ext.optionalPermissions);
    say(@"sites : %@", ext.allRequestedMatchPatterns);
    say(@"arrière-plan : %d, persistant : %d", ext.hasBackgroundContent, ext.hasPersistentBackgroundContent);

    WKWebExtensionControllerConfiguration *cc = [WKWebExtensionControllerConfiguration defaultConfiguration];
    WKWebViewConfiguration *wc = cc.webViewConfiguration;
    [wc.userContentController addUserScript:[[WKUserScript alloc] initWithSource:kCatch injectionTime:WKUserScriptInjectionTimeAtDocumentStart forMainFrameOnly:NO]];
    cc.webViewConfiguration = wc;
    controller = [[WKWebExtensionController alloc] initWithConfiguration:cc];
    delegate = [Del new];
    controller.delegate = delegate;

    context = [WKWebExtensionContext contextForExtension:ext];
    context.uniqueIdentifier = name;
    context.inspectable = YES;
    for (WKWebExtensionPermission p in ext.requestedPermissions)
        [context setPermissionStatus:WKWebExtensionContextPermissionStatusGrantedExplicitly forPermission:p];
    for (WKWebExtensionMatchPattern *m in ext.allRequestedMatchPatterns)
        [context setPermissionStatus:WKWebExtensionContextPermissionStatusGrantedExplicitly forMatchPattern:m];

    // L'onglet, comme dans Feather : la configuration porte le contrôleur.
    WKWebViewConfiguration *tc = [WKWebViewConfiguration new];
    tc.webExtensionController = controller;
    window = [[NSWindow alloc] initWithContentRect:NSMakeRect(100, 100, 1100, 750) styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskResizable backing:NSBackingStoreBuffered defer:NO];
    tab = [Tab new];
    tab.web = [[WKWebView alloc] initWithFrame:NSMakeRect(0, 0, 1100, 750) configuration:tc];
    window.contentView = tab.web;
    [window makeKeyAndOrderFront:nil];
    win = [Win new];
    win.tab = tab;
    tab.win = win;
    delegate.win = win;

    NSError *error = nil;
    [controller loadExtensionContext:context error:&error];
    say(@"chargement : %@ ; adresse de base %@", error ?: @"réussi", context.baseURL);
    [context loadBackgroundContentWithCompletionHandler:^(NSError *e) { say(@"arrière-plan démarré : %@", e ?: @"oui"); }];
    after(4, ^{ probeBackground(context, @"4 s"); });
    [controller didOpenWindow:win];
    [controller didFocusWindow:win];
    [controller didOpenTab:tab];
    [controller didActivateTab:tab previousActiveTab:nil];
    [tab.web loadRequest:[NSURLRequest requestWithURL:[NSURL URLWithString:@"https://account.proton.me/login"]]];

    after(8, ^{
        say(@"erreurs du contexte : %@", context.errors);
        say(@"autorisations accordées : %@", context.currentPermissions);
        say(@"accès à tous les sites : %d", context.hasAccessToAllURLs);
        [controller didChangeTabProperties:WKWebExtensionTabChangedPropertiesURL | WKWebExtensionTabChangedPropertiesLoading forTab:tab];
        probe(tab.web, @"onglet");
        probeBackground(context, @"8 s");
        WKWebExtensionAction *action = [context actionForTab:tab];
        say(@"bouton : %@, fenêtre : %d, libellé %@", action, action.presentsPopup, action.label);
        [context performActionForTab:tab];
    });
    after(18, ^{
        WKWebExtensionAction *action = delegate.presented ?: [context actionForTab:tab];
        say(@"fenêtre présentée par WebKit : %d", delegate.presented != nil);
        NSPopover *pop = action.popupPopover;
        WKWebView *popup = action.popupWebView;
        say(@"bulle %@ taille %@ ; vue %@", pop, NSStringFromSize(pop.contentSize), popup);
        if (pop) [pop showRelativeToRect:NSMakeRect(20, 20, 30, 30) ofView:tab.web preferredEdge:NSRectEdgeMaxX];
        if (popup) probe(popup, @"fenetre");
        say(@"erreurs du contexte : %@", context.errors);
    });
    after(24, ^{
        NSURL *options = context.optionsPageURL;
        say(@"page d'options : %@", options);
        if (!options) return;
        // Configuration du contexte : celle que WebKit demande pour les pages de l'extension.
        WKWebViewConfiguration *oc = context.webViewConfiguration;
        say(@"configuration du contexte : %@ (contrôleur %@)", oc, oc.webExtensionController);
        if (!oc) { oc = [WKWebViewConfiguration new]; oc.webExtensionController = controller; }
        [oc.userContentController addUserScript:[[WKUserScript alloc] initWithSource:kCatch injectionTime:WKUserScriptInjectionTimeAtDocumentStart forMainFrameOnly:NO]];
        optionsWindow = [[NSWindow alloc] initWithContentRect:NSMakeRect(150, 150, 1000, 700) styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
        optionsView = [[WKWebView alloc] initWithFrame:NSMakeRect(0, 0, 1000, 700) configuration:oc];
        optionsWindow.contentView = optionsView;
        [optionsWindow makeKeyAndOrderFront:nil];
        [optionsView loadRequest:[NSURLRequest requestWithURL:options]];
    });
    after(34, ^{
        if (optionsView) probe(optionsView, @"options");
        WKWebView *popup = (delegate.presented ?: [context actionForTab:tab]).popupWebView;
        if (popup) probe(popup, @"fenetre-tard");
        probeBackground(context, @"34 s");
    });
    // Journal de l'arrière-plan (copies enveloppées par le flux de travail).
    after(36, ^{
        NSURL *page = [context.baseURL URLByAppendingPathComponent:@"sonde-page.html"];
        say(@"page du journal : %@", page);
        logView = [[WKWebView alloc] initWithFrame:NSMakeRect(0, 0, 800, 400) configuration:context.webViewConfiguration];
        [logView loadRequest:[NSURLRequest requestWithURL:page]];
    });
    after(39, ^{
        if (logView) [logView evaluateJavaScript:@"document.body.innerText" completionHandler:^(id r, NSError *e) { say(@"journal de l'arrière-plan :\n%@%@", r ?: @"", e ? e.userInfo : @""); }];
    });
    after(41, ^{
        say(@"erreurs du contexte (fin) : %@", context.errors);
        [NSApp terminate:nil];
    });
}

static void uncaught(NSException *e) {
    say(@"exception : %@ %@\n%@", e.name, e.reason, e.callStackSymbols);
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc < 4) { say(@"usage : sonde <dossier> <nom> <sortie>"); return 2; }
        NSString *dir = [NSString stringWithUTF8String:argv[1]];
        name = [NSString stringWithUTF8String:argv[2]];
        outDir = [NSString stringWithUTF8String:argv[3]];
        [[NSFileManager defaultManager] createDirectoryAtPath:outDir withIntermediateDirectories:YES attributes:nil error:nil];
        NSSetUncaughtExceptionHandler(&uncaught);
        say(@"bundle : %@, macOS %@", [NSBundle mainBundle].bundleIdentifier, [NSProcessInfo processInfo].operatingSystemVersionString);
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
        // Adresse absolue, comme dans Feather (une adresse relative fait planter WebKit ?).
        NSString *full = [[[NSFileManager defaultManager] currentDirectoryPath] stringByAppendingPathComponent:dir];
        NSURL *url = [[NSURL fileURLWithPath:full isDirectory:YES] URLByStandardizingPath];
        say(@"lecture de %@", url);
        dirURL = url;
        [WKWebExtension extensionWithResourceBaseURL:url completionHandler:^(WKWebExtension *ext, NSError *error) {
            if (!ext) { say(@"extension refusée : %@", error); [NSApp terminate:nil]; return; }
            run(ext);
        }];
        [NSApp run];
    }
    return 0;
}
