#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <unistd.h>
#include <dlfcn.h>

// llmbutton — Phase 2: READ-ONLY probe.
//
// Confirms that xochitl's QtQuick class names (SceneController, DocumentView,
// SceneSelectionHandler, SelectionContextualMenu, ...) resolve the same way on this
// Paper Pro (Qt 6.11) as they did on the rM2 (Qt 6.x) that inkling was built against,
// before Phase 3 risks injecting a UI element. Nothing is mutated: this just walks the
// live QtQuick visual tree periodically and logs what it finds to stderr (captured by
// `journalctl -u xochitl`).
//
// Pattern and mangled dlsym symbol names lifted from nathanmarlor/inkling's
// xovi-ext/inklingfb/main.c (MIT). Those symbols are Itanium C++ mangled Qt6 API names —
// architecture-independent, so they should resolve identically on aarch64.

// AAPCS64 note (the arm32 sret trick from inkling does NOT transfer as-is): a struct
// >16 bytes returned by value goes back via a HIDDEN POINTER IN REGISTER X8, separate
// from the normal x0..x7 argument registers — unlike arm32, where that hidden pointer
// is just the first normal argument (r0). Declaring these as genuine C struct-return
// functions lets the compiler emit the correct aarch64 convention for us; treating them
// as `void f(void *out, ...)` (arm32-style) silently reads garbage — confirmed on
// device 2026-07-07: allWindows() "succeeded" (no crash) but always returned windows=0
// until fixed. QList<T> is {QTypedArrayData<T>* d; T* ptr; qsizetype size;} = 24 bytes.
typedef struct { void *d, *ptr; long size; } QListRet;
// QVariant has a non-trivial dtor, so per the Itanium C++ ABI it is ALWAYS returned via
// a hidden pointer regardless of its actual size (unlike plain-old-data structs, where
// only >16-byte aggregates go through memory). Declaring a deliberately oversized
// (64-byte) return struct guarantees OUR caller-side classifies it as memory/x8-sret
// too, matching the callee — we don't need to know QVariant's exact real size, only
// that ours is unambiguously above the 16-byte register-return threshold.
typedef struct { char buf[64]; } QVarRet;
typedef const char *(*classname_fn)(const void*);
typedef void        (*findchildren_fn)(const void*, const void*, void*, int);  // real out-param, not sret — fine as-is
typedef QListRet    (*allwindows_fn)(void);
typedef QListRet    (*childitems_fn)(const void*);
typedef QVarRet     (*qproperty_fn)(const void*, const char*);        // QObject::property(const char*) -> QVariant
typedef char        (*invokeimpl_fn)(void*, void*, int, void*);       // QMetaObject::invokeMethodImpl(obj, slotObj, type, ret)
typedef char        (*v_tobool_fn)(const void*);                      // QVariant::toBool() -- primitive return, no sret issue
typedef double      (*v_todouble_fn)(const void*, void*);             // QVariant::toDouble(bool*) -- primitive return
typedef void        (*v_dtor_fn)(void*);                              // ~QVariant()
// --- runtime QML injection (the LLM button) --- all constructors/void/pointer
// returns below -- none of these are by-value-aggregate returns, so the aarch64
// hidden-sret-in-x8 issue above does not apply to any of them.
typedef void* (*qmlengine_fn)(const void*);    // qmlEngine(QObject*) -> QQmlEngine*
typedef void* (*qmlcontext_fn)(const void*);   // qmlContext(QObject*) -> QQmlContext*
typedef void  (*qcomp_ctor_fn)(void*, void*, void*);                   // QQmlComponent(engine, parent)
typedef void  (*qcomp_setdata_fn)(void*, const void*, const void*);    // setData(QByteArray, QUrl)
typedef void* (*qcomp_create_fn)(void*, void*);                        // create(QQmlContext*)
typedef int   (*qcomp_status_fn)(const void*);                         // QQmlComponent::status() -> enum (Null=0,Ready=1,Loading=2,Error=3)
typedef void  (*qba_ctor_fn)(void*, const char*, long);                // QByteArray(const char*, qsizetype)
typedef void  (*qurl_ctor_fn)(void*);                                  // QUrl()
typedef void  (*qurl_dtor_fn)(void*);
typedef char  (*setprop_fn)(void*, const char*, const void*);          // QObject::setProperty(name, QVariant)
typedef void  (*qvar_i_fn)(void*, int);                                // QVariant(int)
typedef void  (*setparentitem_fn)(void*, void*);                       // QQuickItem::setParentItem
typedef void  (*setparent_fn)(void*, void*);                           // QObject::setParent

static classname_fn    p_className;
static findchildren_fn p_findchildren;
static allwindows_fn   p_allwindows;
static childitems_fn   p_childitems;
static qproperty_fn    p_qproperty;
static invokeimpl_fn   p_invokeimpl;
static v_tobool_fn     p_v_tobool;
static v_todouble_fn   p_v_todouble;
static v_dtor_fn       p_v_dtor;
static qmlengine_fn     p_qmlengine;
static qmlcontext_fn    p_qmlcontext;
static qcomp_ctor_fn    p_qcomp_ctor;
static qcomp_setdata_fn p_qcomp_setdata;
static qcomp_create_fn  p_qcomp_create;
static qcomp_status_fn  p_qcomp_status;
static qba_ctor_fn      p_qba_ctor;
static qurl_ctor_fn     p_qurl_ctor;
static qurl_dtor_fn     p_qurl_dtor;
static setprop_fn       p_setprop;
static qvar_i_fn        p_qvar_i;
static setparentitem_fn p_setparentitem;
static setparent_fn     p_setparent;
static void           **p_qapp_self;   // &QCoreApplication::self
static void            *g_qobj_smo;    // &QObject::staticMetaObject

// --- tiny Qt introspection helpers (all read-only, safe on any QObject) ---
static void* meta_of(void *o){ return ((void*(*)(void*))(*(void***)o)[0])(o); }   // metaObject() @ vtable[0]
static const char* cls(void *o){ if(!o) return 0; void *mo=meta_of(o); return mo?p_className(mo):0; }
static void* read_obj_prop(void *o, const char *name){
    QVarRet v = p_qproperty(o, name);
    void *ptr = *(void**)v.buf;         // ptr stored inline in QVariant, if this is a pointer-valued property
    if(p_v_dtor) p_v_dtor(v.buf);
    return ptr;
}
static double read_obj_double(void *o, const char *name){
    QVarRet v = p_qproperty(o, name);
    double d = p_v_todouble ? p_v_todouble(v.buf, 0) : 0.0;
    if(p_v_dtor) p_v_dtor(v.buf);
    return d;
}
static int read_obj_bool(void *o, const char *name){
    QVarRet v = p_qproperty(o, name);
    int b = p_v_tobool ? p_v_tobool(v.buf) : 0;
    if(p_v_dtor) p_v_dtor(v.buf);
    return b;
}
static int is_quickitem(void *o){
    void *mo=meta_of(o);
    for(int i=0; mo && i<40; i++){ const char*c=p_className(mo); if(c&&!strcmp(c,"QQuickItem")) return 1; mo=*(void**)mo; }
    return 0;
}

// --- collect the active page's SceneController(s) + scene views + selection-ish items ---
static void *g_seen[9000]; static int g_nseen;
static void *g_scs[16];    static int g_nscs;
static void *g_views[16];  static int g_nviews;
static void *g_selitems[32]; static int g_nselitems;
static int seen(void *p){ for(int i=0;i<g_nseen;i++) if(g_seen[i]==p) return 1; if(g_nseen<9000) g_seen[g_nseen++]=p; return 0; }

static void add_sc(void *sc){
    if(!sc){ return; } const char *c=cls(sc); if(!c||strcmp(c,"SceneController")) return;
    for(int i=0;i<g_nscs;i++) if(g_scs[i]==sc) return;
    if(g_nscs<16) g_scs[g_nscs++]=sc;
}
static void add_view(void *v){ for(int i=0;i<g_nviews;i++) if(g_views[i]==v) return; if(g_nviews<16) g_views[g_nviews++]=v; }

static void walk(void *item, int depth){
    if(!item || depth>70 || g_nseen>8000 || seen(item)) return;
    const char *c = cls(item); if(!c) return;
    if(!strcmp(c,"SceneController")){ add_sc(item); return; }
    if((strstr(c,"Select")||strstr(c,"select")) && g_nselitems<32) g_selitems[g_nselitems++]=item;
    if(strstr(c,"DocumentView") && !strstr(c,"Shortcuts")){ add_sc(read_obj_prop(item,"sceneController")); add_view(item); }
    else if(strstr(c,"DeviceScene")){ add_sc(read_obj_prop(item,"controller")); add_view(item); }
    if(is_quickitem(item)){
        QListRet cl = p_childitems(item);
        void **kids=(void**)cl.ptr; long kn=cl.size;
        for(long i=0;i<kn;i++) walk(kids[i], depth+1);
    }
}

static long g_last_wn, g_last_n, g_last_rootitems;
static void locate(void){
    g_nseen=0; g_nscs=0; g_nviews=0; g_nselitems=0;
    QListRet wl = p_allwindows();
    void **wins=(void**)wl.ptr; long wn=wl.size;
    g_last_wn = wn; g_last_n = 0; g_last_rootitems = 0;
    for(long w=0; w<wn; w++){
        const char *wc = cls(wins[w]);
        void *lst[3]={0,0,0}; p_findchildren(wins[w], g_qobj_smo, lst, 1);   // find QQuickRootItem
        void **arr=(void**)lst[1]; long n=(long)lst[2];
        g_last_n += n;
        fprintf(stderr, "[llmbutton]   win[%ld]=%s findChildren=%ld\n", w, wc?wc:"?", n);
        for(long j=0;j<n;j++){
            const char *c=cls(arr[j]);
            if(j<8) fprintf(stderr, "[llmbutton]     child[%ld] = %s\n", j, c?c:"?");
            if(c && !strcmp(c,"QQuickRootItem")){ g_last_rootitems++; walk(arr[j],0); }
        }
    }
}

static void gui_add_llm_button(void *menu);
static void probe_once(void){
    locate();
    fprintf(stderr, "[llmbutton] probe: windows=%ld foundChildren=%ld rootItems=%ld seen=%d scs=%d views=%d selitems=%d\n",
            g_last_wn, g_last_n, g_last_rootitems, g_nseen, g_nscs, g_nviews, g_nselitems);
    for(int i=0;i<g_nviews;i++) fprintf(stderr, "[llmbutton]   view[%d] = %s\n", i, cls(g_views[i]));
    void *menu = 0;
    for(int i=0;i<g_nselitems;i++){
        const char *c = cls(g_selitems[i]);
        fprintf(stderr, "[llmbutton]   selitem[%d] = %s\n", i, c ? c : "?");
        if(c && strstr(c,"SelectionContextualMenu")) menu = g_selitems[i];
    }
    if(menu) gui_add_llm_button(menu);
}

static void set_prop_int(void *o, const char *n, int v){
    char var[64]; for(int k=0;k<64;k++) var[k]=0; p_qvar_i(var, v);
    p_setprop(o, n, var); if(p_v_dtor) p_v_dtor(var);
}

// The button's QML: a plain-text "LLM" label (not "AI") beside the selection menu.
// TapHandler (not MouseArea) matches xochitl's own menu buttons -- the touchscreen
// delivers raw touch with no mouse synthesis, so MouseArea never fires (inkling's
// finding, reused as-is). onTapped just flips a dynamic property; the C side polls it
// on the next heartbeat and does the actual file write -- keeps all real I/O out of the
// QML JS sandbox and reuses only already-proven-safe property read/write calls.
static const char *LLM_QML =
    "import QtQuick\n"
    "Rectangle {\n"
    "  property bool llmMark: true\n"
    "  property bool llmTapped: false\n"
    "  width: 84; height: 84\n"
    "  color: \"white\"\n"
    "  border.color: \"black\"; border.width: 2\n"
    "  Text { anchors.centerIn: parent; text: \"LLM\"; font.pixelSize: 22; font.bold: true; color: \"black\" }\n"
    "  TapHandler {\n"
    "    gesturePolicy: TapHandler.ReleaseWithinBounds\n"
    "    onTapped: llmTapped = true\n"
    "  }\n"
    "}\n";

// Sibling of LLM_QML: same shape, a different mark/tapped property pair
// (drawMark/drawTapped) so the two buttons' identity checks never collide,
// and a different label. Placed one button-width + gap further right.
static const char *DRAW_QML =
    "import QtQuick\n"
    "Rectangle {\n"
    "  property bool drawMark: true\n"
    "  property bool drawTapped: false\n"
    "  width: 84; height: 84\n"
    "  color: \"white\"\n"
    "  border.color: \"black\"; border.width: 2\n"
    "  Text { anchors.centerIn: parent; text: \"Draw\"; font.pixelSize: 20; font.bold: true; color: \"black\" }\n"
    "  TapHandler {\n"
    "    gesturePolicy: TapHandler.ReleaseWithinBounds\n"
    "    onTapped: drawTapped = true\n"
    "  }\n"
    "}\n";

#define LLM_TRIGGER_FILE "/tmp/llm_button_trigger"
#define DRAW_TRIGGER_FILE "/tmp/draw_button_trigger"
#define BUTTON_W 84.0
#define BUTTON_GAP 12.0

// Placement-storage sizes for raw Qt object ABI hacking below. Deliberately
// oversized rather than hand-measured (matching QVarRet's approach for
// QVariant elsewhere in this file): QByteArray alone was previously
// undersized at 16 bytes when its real Qt6 layout is 24 bytes (same
// {Data*, char*, qsizetype} shape as QListRet, per the comment on that
// type above) -- silently corrupting the QByteArray handed to setData()
// and making every injected button's QML fail to compile
// (QQmlComponent::status() == Error) despite the QML text itself being
// fine. An oversized buffer can never corrupt anything (we zero it and
// only ever read back what the real object wrote), so there's no
// downside to being generous here.
#define QCOMP_BUF_SZ 128
#define QBA_BUF_SZ 64
#define QURL_BUF_SZ 64

// Sibling-button injector, parameterized over the QML source, the dynamic
// "mark"/"tapped" property names, the trigger file, and the x-offset from the
// menu's right edge (so several buttons can be lined up in a row). The
// QQmlComponent/QByteArray statics are owned by the CALLER (one dedicated
// static pair per button -- see gui_add_llm_button below), not shared across
// buttons, so this is exactly the original single-button logic with the
// button-specific bits pulled out as parameters -- nothing new is shared
// state between the LLM and Draw buttons.
// GUI THREAD ONLY (called from probe_once, itself only ever invoked via run_on_gui).
static void gui_add_button(void *menu, const char *qml, const char *mark_prop, const char *tapped_prop,
                           const char *trigger_file, double x_offset,
                           char *comp, void **comp_engine, char *ba, int *ba_init){
    void *handler = read_obj_prop(menu, "parent");
    if(!handler){ fprintf(stderr, "[llmbutton] add-btn: menu has no parent\n"); return; }
    double mx = read_obj_double(menu, "x");
    double my = read_obj_double(menu, "y");
    double mw = read_obj_double(menu, "width"); if(mw < 1.0) mw = 324.0;
    int    mvis = read_obj_bool(menu, "visible");
    double bx = mx + mw + x_offset;

    { // existing button on the handler? track menu box + poll its tapped flag, then done.
        QListRet cl = p_childitems(handler);
        void **kids=(void**)cl.ptr; long kn=cl.size;
        for(long i=0;i<kn;i++){
            if(read_obj_bool(kids[i], mark_prop)){
                set_prop_int(kids[i], "x", (int)bx);
                set_prop_int(kids[i], "y", (int)my);
                set_prop_int(kids[i], "visible", mvis ? 1 : 0);
                if(read_obj_bool(kids[i], tapped_prop)){
                    fprintf(stderr, "[llmbutton] %s tapped! writing trigger file\n", mark_prop);
                    FILE *tf = fopen(trigger_file, "w"); if(tf) fclose(tf);
                    set_prop_int(kids[i], tapped_prop, 0);
                }
                return;
            }
        }
    }

    if(!p_qmlengine || !p_qcomp_ctor || !p_qba_ctor || !p_qurl_ctor || !p_qcomp_setdata || !p_qcomp_create){
        fprintf(stderr, "[llmbutton] add-btn: missing required symbol (engine=%p ctor=%p ba=%p url=%p setdata=%p create=%p) -- refusing to inject\n",
                (void*)p_qmlengine, (void*)p_qcomp_ctor, (void*)p_qba_ctor, (void*)p_qurl_ctor, (void*)p_qcomp_setdata, (void*)p_qcomp_create);
        return;
    }
    void *engine = p_qmlengine(menu);
    if(!engine){ fprintf(stderr, "[llmbutton] add-btn: no qml engine\n"); return; }
    if(*comp_engine != engine){
        for(int k=0;k<QCOMP_BUF_SZ;k++) comp[k]=0;
        p_qcomp_ctor(comp, engine, 0);
        if(!*ba_init){
            for(int k=0;k<QBA_BUF_SZ;k++) ba[k]=0;
            p_qba_ctor(ba, qml, (long)strlen(qml));
            *ba_init = 1;
        }
        char url[QURL_BUF_SZ]; for(int k=0;k<QURL_BUF_SZ;k++) url[k]=0; p_qurl_ctor(url);
        p_qcomp_setdata(comp, ba, url);
        if(p_qurl_dtor) p_qurl_dtor(url);
        *comp_engine = engine;

        // setData() may compile asynchronously (status Loading) rather than
        // finishing synchronously -- create() returns null if called before
        // the component reaches Ready, so poll briefly (GUI thread only,
        // this whole call chain is already on it) before giving up.
        if(p_qcomp_status){
            for(int w=0; w<50; w++){
                int st = p_qcomp_status(comp);
                if(st != 2 /*Loading*/) break;
                usleep(2000);
            }
        }
    }
    int status_before = p_qcomp_status ? p_qcomp_status(comp) : -1;
    void *item = p_qcomp_create(comp, p_qmlcontext ? p_qmlcontext(menu) : 0);
    if(item && p_setparentitem && p_setparent){
        set_prop_int(item, "x", (int)bx);
        set_prop_int(item, "y", (int)my);
        p_setparent(item, handler);        // QObject ownership: dies with the handler
        p_setparentitem(item, handler);    // visual SIBLING of the menu, not content
        fprintf(stderr, "[llmbutton] button (%s) attached item=%p handler=%p at (%.0f,%.0f)\n",
                mark_prop, item, handler, bx, my);
    } else fprintf(stderr, "[llmbutton] add-btn: component create failed (item=%p) status=%d (0=Null 1=Ready 2=Loading 3=Error)\n",
                   item, status_before);
}

static void gui_add_llm_button(void *menu){
    static char llm_comp[QCOMP_BUF_SZ]; static void *llm_comp_engine = 0;
    static char llm_ba[QBA_BUF_SZ]; static int llm_ba_init = 0;
    gui_add_button(menu, LLM_QML, "llmMark", "llmTapped", LLM_TRIGGER_FILE, BUTTON_GAP,
                   llm_comp, &llm_comp_engine, llm_ba, &llm_ba_init);

    static char draw_comp[QCOMP_BUF_SZ]; static void *draw_comp_engine = 0;
    static char draw_ba[QBA_BUF_SZ]; static int draw_ba_init = 0;
    gui_add_button(menu, DRAW_QML, "drawMark", "drawTapped", DRAW_TRIGGER_FILE, BUTTON_GAP + BUTTON_W + BUTTON_GAP,
                   draw_comp, &draw_comp_engine, draw_ba, &draw_ba_init);
}

// --- GUI-thread executor -----------------------------------------------------------
// ALL Qt access must happen on xochitl's GUI thread (posted via the exported
// QMetaObject::invokeMethodImpl(QObject*, QSlotObjectBase*, ConnectionType, void*)).
// Calling Qt introspection directly from our own pthread crashed xochitl immediately
// (confirmed on-device 2026-07-07: SIGSEGV within the first probe, twice, before it
// ever logged a result) — this is inkling's #1 "paid for with a crash" rule, restored.
typedef void (*gui_job_fn)(void);
static volatile gui_job_fn g_gui_job;
static void gui_exec_impl(void *a1, void *a2, void **a3, int a4, char *a5){
    (void)a2;(void)a3;(void)a5;
    int which = ((unsigned long)a1 < 8ul) ? (int)(unsigned long)a1 : a4;
    if(which==1){ gui_job_fn f = g_gui_job; if(f){ g_gui_job = 0; f(); } }
}
static void *g_exec_slot[4] = { (void*)gui_exec_impl, (void*)gui_exec_impl, 0, 0 };
static int run_on_gui(gui_job_fn f){
    void *app = p_qapp_self ? *p_qapp_self : 0;
    if(!p_invokeimpl || !app) return 0;
    g_gui_job = f;
    p_invokeimpl(app, g_exec_slot, 2 /*QueuedConnection*/, 0);
    for(int i=0;i<50 && g_gui_job;i++) usleep(20000);   // wait <=1s for the job to run
    return g_gui_job == 0;
}

static void* watcher(void* _){
    (void)_;
    for(;;){
        if(!run_on_gui(probe_once)) fprintf(stderr, "[llmbutton] probe: GUI job did not complete\n");
        sleep(3);
    }
    return 0;
}

void _xovi_construct(void){
    p_className   = (classname_fn) dlsym(RTLD_DEFAULT,"_ZNK11QMetaObject9classNameEv");
    p_findchildren= (findchildren_fn) dlsym(RTLD_DEFAULT,"_Z23qt_qFindChildren_helperPK7QObjectRK11QMetaObjectP5QListIPvE6QFlagsIN2Qt15FindChildOptionEE");
    p_allwindows  = (allwindows_fn) dlsym(RTLD_DEFAULT,"_ZN15QGuiApplication10allWindowsEv");
    p_childitems  = (childitems_fn) dlsym(RTLD_DEFAULT,"_ZNK10QQuickItem10childItemsEv");
    p_qproperty   = (qproperty_fn) dlsym(RTLD_DEFAULT,"_ZNK7QObject8propertyEPKc");
    p_invokeimpl  = (invokeimpl_fn) dlsym(RTLD_DEFAULT,"_ZN11QMetaObject16invokeMethodImplEP7QObjectPN9QtPrivate15QSlotObjectBaseEN2Qt14ConnectionTypeEPv");
    p_qapp_self   = (void**)        dlsym(RTLD_DEFAULT,"_ZN16QCoreApplication4selfE");
    p_v_tobool    = (v_tobool_fn)   dlsym(RTLD_DEFAULT,"_ZNK8QVariant6toBoolEv");
    p_v_todouble  = (v_todouble_fn) dlsym(RTLD_DEFAULT,"_ZNK8QVariant8toDoubleEPb");
    p_v_dtor      = (v_dtor_fn)     dlsym(RTLD_DEFAULT,"_ZN8QVariantD1Ev");
    p_qmlengine   = (qmlengine_fn)   dlsym(RTLD_DEFAULT,"_Z9qmlEnginePK7QObject");
    p_qmlcontext  = (qmlcontext_fn)  dlsym(RTLD_DEFAULT,"_Z10qmlContextPK7QObject");
    p_qcomp_ctor  = (qcomp_ctor_fn)  dlsym(RTLD_DEFAULT,"_ZN13QQmlComponentC1EP10QQmlEngineP7QObject");
    p_qcomp_setdata=(qcomp_setdata_fn)dlsym(RTLD_DEFAULT,"_ZN13QQmlComponent7setDataERK10QByteArrayRK4QUrl");
    p_qcomp_create= (qcomp_create_fn)dlsym(RTLD_DEFAULT,"_ZN13QQmlComponent6createEP11QQmlContext");
    p_qcomp_status= (qcomp_status_fn)dlsym(RTLD_DEFAULT,"_ZNK13QQmlComponent6statusEv");
    // QByteArray's char* ctor's size-parameter width has moved across Qt6 minor
    // versions (int in older Qt6, qsizetype -- long or long long depending on build --
    // in newer ones); dlsym'ing the wrong mangled name returns NULL rather than a
    // crash, so try each candidate and keep the first that resolves. Confirmed on
    // device 2026-07-08: the 'i'-suffixed (int) symbol inkling used doesn't exist on
    // this Qt 6.11 build -- calling through the resulting NULL pointer crashed
    // xochitl the instant a selection menu was found. Argument WIDTH mismatch alone
    // (int vs long) would NOT have crashed -- register-passed integers under 2^31
    // read correctly regardless -- only a missing symbol does.
    p_qba_ctor = (qba_ctor_fn) dlsym(RTLD_DEFAULT,"_ZN10QByteArrayC1EPKci");
    if(!p_qba_ctor) p_qba_ctor = (qba_ctor_fn) dlsym(RTLD_DEFAULT,"_ZN10QByteArrayC1EPKcx");
    if(!p_qba_ctor) p_qba_ctor = (qba_ctor_fn) dlsym(RTLD_DEFAULT,"_ZN10QByteArrayC1EPKcl");
    p_qurl_ctor   = (qurl_ctor_fn)   dlsym(RTLD_DEFAULT,"_ZN4QUrlC1Ev");
    p_qurl_dtor   = (qurl_dtor_fn)   dlsym(RTLD_DEFAULT,"_ZN4QUrlD1Ev");
    p_setprop     = (setprop_fn)     dlsym(RTLD_DEFAULT,"_ZN7QObject11setPropertyEPKcRK8QVariant");
    p_qvar_i      = (qvar_i_fn)      dlsym(RTLD_DEFAULT,"_ZN8QVariantC1Ei");
    p_setparentitem=(setparentitem_fn)dlsym(RTLD_DEFAULT,"_ZN10QQuickItem13setParentItemEPS_");
    p_setparent   = (setparent_fn)   dlsym(RTLD_DEFAULT,"_ZN7QObject9setParentEPS_");
    g_qobj_smo    = dlsym(RTLD_DEFAULT,"_ZN7QObject16staticMetaObjectE");

    unlink(LLM_TRIGGER_FILE);   // never act on a stale trigger left over from a previous run
    unlink(DRAW_TRIGGER_FILE);

    fprintf(stderr, "[llmbutton] loaded (probe+button) symbols: className=%p findChildren=%p allWindows=%p childItems=%p property=%p invokeImpl=%p qappSelf=%p v_tobool=%p v_todouble=%p v_dtor=%p qmlEngine=%p qmlContext=%p qcompCtor=%p qcompSetData=%p qcompCreate=%p qbaCtor=%p urlCtor=%p urlDtor=%p setProp=%p qvarI=%p setParentItem=%p setParent=%p smo=%p\n",
            (void*)p_className, (void*)p_findchildren, (void*)p_allwindows, (void*)p_childitems, (void*)p_qproperty,
            (void*)p_invokeimpl, (void*)p_qapp_self, (void*)p_v_tobool, (void*)p_v_todouble, (void*)p_v_dtor,
            (void*)p_qmlengine, (void*)p_qmlcontext, (void*)p_qcomp_ctor, (void*)p_qcomp_setdata, (void*)p_qcomp_create,
            (void*)p_qba_ctor, (void*)p_qurl_ctor, (void*)p_qurl_dtor, (void*)p_setprop, (void*)p_qvar_i,
            (void*)p_setparentitem, (void*)p_setparent, g_qobj_smo);

    pthread_t t; pthread_create(&t, NULL, watcher, NULL);
}
