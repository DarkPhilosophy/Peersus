# Contract de arhitectură și interfață

## Construirea proiectului și sursa unică de adevăr

Rust workspace (edition >2024-2026, MSRV 1.96). Dependencies are declared once in the root `Cargo.toml` and referenced with `workspace = true`. `unsafe_code` is forbidden except in the two Android platform crates below, and there only for narrow, audited FFI boundaries.

| Componentă responsabilă | Responsabilitate |
|---|---|
| `crates/peersus-api` | Contractul comun dintre extensii și aplicație, folosit de ambele: manifest, servicii, contribuții, setări, elementele paginilor (`elements`) și mesaje. Doar date și validare: fără broker, procese, toolkit, sistem de operare, rețea sau concepte de produs. |
| `crates/peersus-plugin` | Tot ce rulează pluginurile, al aplicației: brokerul (`hub`) care rutează apelurile și evenimentele, supravegherea proceselor, cadrele și conexiunile de date. Vorbește doar contractul din `peersus-api`. |
| `crates/peersus-core` | Modelul produsului, comenzile, asocierea și setările. Nu cunoaște niciun plugin concret. |
| `crates/peersus-net` | Identitate, transport criptat, transfer și coduri QR. |
| `crates/peersus-app` | Responsabil de execuție, CLI, IPC local, blocarea instanței și gazda pluginurilor. |
| `crates/peersus-ui` | Freya frontend (library). Renders `Snapshot` and plugin `Contributions`; owns no business logic. |
| `crates/peersus-ui-android` | Punct de intrare al aplicației Android și adaptoare de platformă JNI. Doar FFI restrâns și auditat. |
| `crates/peersus-plugin-android` | Adaptor JNI pentru descriptorul lucrătorului WASM izolat. Conectează doar interpretorul cu limite definite; nu include interfață grafică, identitatea aplicației, stiva de rețea sau un încărcător nativ extern. Doar FFI restrâns și auditat. |
| `plugins/` | Bundled plugin adapters (clipboard first). Depend on `peersus-api` only. |

## Builduri de dezvoltare și structura artefactelor

- Project stage: **private, unpublished development prerelease**. Cargo `debug`/`release` profiles are compilation choices, not permission to publish, tag or change the project version.

- Preferred entry point: `./dev.sh`. It delegates to `cargo xtask`; do not maintain a second set of ad-hoc build commands.

- `./dev.sh desktop` builds the desktop programs, bundled extensions and native helper in the Cargo **dev** profile (`debug/`).

- `./dev.sh android` builds community native libraries for both declared Android ABIs, then packages a **debuggable community APK** with Gradle's existing local debug signer. The Rust Android libraries remain optimized (`release`); a debug APK does not imply unoptimized Rust.

- `./dev.sh desktop --container NAME` or `./dev.sh android --container NAME` uses an existing Distrobox for compilation. Android APK packaging/debug signing stays on the host, avoiding an unrelated container debug certificate.

- `./dev.sh deploy nx rog` explicitly updates only those selected physical phones. It builds nothing, deduplicates multiple ADB transports, checks every installed signer before any installation, uses replace-install and verifies the installed APK byte-for-byte. It never uninstalls or clears application data; a signer mismatch is a refusal, not a reason to erase data.

- `./build.sh desktop` selects optimized desktop release artifacts; `./build.sh android` prepares both unsigned Android release flavors and bundles. Build commands never deploy.

- `./check.sh` runs the existing strict verification gate. Individual existing gates remain available as arguments.

- Compilează sursa modificată prin aceste puncte de intrare; confirmă că au fost recompilate crate-urile afectate și artefactele cerute pentru platformă. Un fișier sursă salvat, un APK vechi sau compilarea reușită a unei alte ținte nu reprezintă dovezi că buildul curent este valid.

- Buildul și deploy-ul sunt operațiuni separate. Fă deploy pe fiecare telefon solicitat explicit de utilizator, nu doar pe primul transport conectat. Dacă o țintă solicitată nu este disponibilă, operațiunea rămâne în așteptare și trebuie raportată; alt telefon nu o poate înlocui.

- When an extension archive is requested in a phone's `Download/`, copy the actual platform-compatible archive there and verify its remote size and checksum against the local artifact. A desktop copy, an installed extension or a previous transfer does not establish that the file exists on that phone.

Canonical helper layout is owned by `scripts/build_layout.py`, not hand-written OS aliases. Desktop helpers set the native `CARGO_BUILD_TARGET`; Cargo itself creates the standard target-triple directory:

```text

target/ui/\<rust-target-triple>/debug/programe și utilitare desktop pentru dezvoltare

target/ui/\<rust-target-triple>/release/programe și utilitare desktop optimizate

target/android/\<android-rust-target-triple>/release/  artefacte Rust optimizate pentru Android

target/android/jniLibs/\<flavor>/\<android-abi>/fișiere de intrare pentru etapa de pregătire a bibliotecilor native Gradle

target/android/gradle/cache-uri Gradle, fișiere intermediare și rezultate brute

target/distribution/android/community/debug/APK de dezvoltare instalabil

```

The development APK is `target/distribution/android/community/debug/app-community-debug.apk`. `CARGO_TARGET_DIR` and an explicit `CARGO_BUILD_TARGET` remain overrides; consumers must use the same environment and resolver. Bare Cargo retains its normal native `target/\<profile>/` convention unless those variables are set. Never silently select an older flat-directory artifact when a triple-specific artifact is missing. Changing the default layout does not migrate, delete or overwrite old caches. Windows/macOS triples describe layout; their appearance here does not claim new cross-compilation or packaging support.

See `docs/development.md` for prerequisites, detailed commands and the distinction between artifacts, staging and published releases.

## Reguli pentru agnosticismul față de pluginuri

- Core-ul și interfața grafică nu menționează niciodată numele unui plugin. Un plugin este identificat printr-un ID atribuit de gazdă, niciodată printr-un câmp din manifest.

- Orice adaugă un plugin produsului (pagini, acțiuni, setări, permisiuni acordate per dispozitiv) este declarat ca date în manifestul său și afișat într-un mod generic.

- Comunicarea dintre pluginuri trece prin broker: importuri/exporturi cu versiuni exacte, liste de permisiuni explicite pentru furnizori și acordarea capabilităților.

- Network streams are routed by `(service, version)` to the provider that exports them; no host code branches on a service name.

- Permisiunile acordate per dispozitiv și setările pluginurilor sunt stocate în hărți generice indexate după ID-ul pluginului.

- Agnosticismul este o proprietate invariantă de la un capăt la altul, nu doar un nume de API: urmărește declarațiile prin gazdă, interfață, adaptorul de platformă și consumatorul efectiv. O proprietate care doar pare generică, dar nu este folosită de niciun consumator, nu implementează o capabilitate.

- Nu introduce în codul comun nume de furnizori, endpointuri, identități de câmpuri sau comportamente specifice pluginurilor. Politicile specifice unui furnizor îi aparțin acestuia; straturile comune consumă capabilități și date explicite, tipizate. Constantele protocolului și valorile implicite documentate aparțin componentei responsabile de contract, nu unor apeluri duplicate în mai multe locuri.

- Reutilizează implementarea existentă înainte de a adăuga un alt utilitar, trait, registru sau adaptor. Aplică principiul DRY comportamentului și responsabilităților, nu doar elementelor cu denumiri similare; nu combina semantici distincte într-o abstractizare încărcată de flaguri.

- Elementele comune ale interfeței, inclusiv câmpurile de text, au o singură cale reutilizabilă de construire și comportament. Punctele de apel furnizează profiluri declarative și date; semantica pentru text normal, secrete, căutare și adresă nu trebuie să devină implementări separate.

- Codul specific platformei rămâne în spatele unei limite de platformă restrânse. Păstrează validarea comună, tranzițiile de stare și contractele extensiilor independente de platformă; traduce evenimentele și capabilitățile native o singură dată, în adaptor.

- Folosește convențiile idiomatice Rust pentru ownership, tipuri și gestionarea erorilor, precum și convențiile existente ale spațiului de lucru. Capabilitățile nesuportate și erorile trebuie să rămână explicite; fără câmpuri fictive, operații fără efect ascunse, soluții de rezervă inventate sau aliasuri de compatibilitate fără un consumator real.

- Păstrează semantica de securitate a fiecărei capabilități în toate straturile. Textul secret nu trebuie să ajungă în loguri, diagnostice, mecanisme de accesibilitate sau fluxuri ale clipboardului; simpla mascarea a textului nu înseamnă suport nativ pentru introducerea secretelor.

- **Izolare dură (HARD GUARD):** o extensie nu poate niciodată să oprească, să blocheze, să încetinească sau să deconecteze aplicația. Orice date venite de la o extensie (manifest, contribuții, răspunsuri, apeluri, evenimente) sunt nesigure: validate la graniță, refuzate explicit când nu sunt înțelese, fără `panic`, `unwrap`/`expect` pe ele, fără bucle sau așteptări fără limită de timp în bucla principală. O extensie care eșuează este oprită sau repornită izolat, cu mesaj fix pentru utilizator; restul aplicației și celelalte extensii merg mai departe.

- **Limitare profesională:** fiecare extensie are cote și limite de rată pentru apeluri, evenimente, notificări, date și conexiuni. Peste limită, cererile ei sunt refuzate sau amânate (rate limited), nu puse în coadă la nesfârșit; o extensie care face spam în mod repetat este suspendată temporar și utilizatorul este anunțat.

- **Stabilitate verificată pe toate dispozitivele:** orice schimbare care atinge gazda de extensii, protocolul sau interfața se testează pe desktop și pe fiecare telefon disponibil (ROG, NX), inclusiv cazuri limită (extensie nouă pe aplicație veche și invers, date necunoscute, permisiuni comutate repetat, repornire, telefon blocat sau în fundal) și un stress test de extensie (spam de apeluri, răspunsuri invalide sau lente, extensie care se blochează sau moare). Acceptarea cere ca aplicația să rămână receptivă și conectată pe toată durata testului.


### HARD RULE — pluginuri opționale, gazdă agnostică

- **Aplicația pornește fără niciun plugin instalat implicit.** Pluginurile externe sunt opționale și se instalează separat. Existența unui plugin în surse sau printre artefactele compilate nu îl transformă într-o funcționalitate implicită a aplicației.
- **Este interzisă hardcodarea unui plugin extern în aplicație.** Numele, ID-urile, cheile setărilor, paginile și comportamentele specifice lui nu devin condiții sau ramuri speciale în UI, core, gazdă ori adaptoarele de platformă. Aplicația le poate transporta și afișa ca date prin contractele generice existente.
- **Orice implementare comună necesară pluginurilor trebuie să fie o capabilitate API agnostică și reutilizabilă**, utilizabilă și de alte pluginuri, inclusiv viitoare, fără modificarea aplicației pentru fiecare plugin. Nu este suficient să redenumești logica unui plugin „API”: contractul și implementarea nu trebuie să depindă de acel plugin, de setările sau de scenariul său particular.
- **Pluginul deține semantica propriilor setări și funcționalități.** UI-ul aplicației doar redă contribuțiile și transmite interacțiunile prin suportul generic. Un checkbox boolean obișnuit reutilizează componenta existentă; nu justifică un checkbox duplicat, logică specifică în renderer sau un câmp API dedicat opțiunii respective.
- **Înaintea unei extinderi, verifică suportul existent și consumatorii reali.** Reutilizează-l dacă acoperă cerința. Dacă lipsește o capabilitate, delimitează explicit API-ul reutilizabil de logica rămasă în plugin; nu introduce pe ascuns o excepție pentru plugin și nu construi infrastructură speculativă fără legătură cu cerința.
- **Verificarea obligatorie urmărește ambele situații:** aplicația fără plugin rămâne funcțională, fără funcționalități sau intrări UI reziduale ale acestuia; cu pluginul instalat, contribuțiile și setările sale funcționează prin aceleași contracte agnostice. Codul specific unui plugin opțional, rămas fără un rol API reutilizabil în aplicație, nu este acceptabil și nu se justifică drept „suport pentru viitor”.

## Contractul de asociere

Ambii utilizatori aleg independent același cerc dintre cele trei cercuri numerotate; dacă alegerile diferă, asocierea este refuzată. Transferul de fișiere nu asociază niciodată implicit un dispozitiv.

## Interfața utilizatorului (decizie luată; migrarea de la prototipul Blitz nu este finalizată)

- Toolkit: **Freya** (`freya`, MIT), din ramura „release candidate” 0.5, fixată la o versiune specifică în fișierul `Cargo.toml` rădăcină. A fost aleasă în urma unei evaluări comparative cu Xilem; argumentele și limitările recunoscute sunt detaliate în notele proiectului și nu vor fi reluate aici. Interfața grafică (UI) constă într-o singură bibliotecă (crate), completată de puncte de intrare minimale (un `main` pentru desktop și un `android_main` pentru Android, în cadrul bibliotecii specifice platformei), astfel încât același set de ecrane este utilizat atât pe desktop, cât și pe dispozitivele mobile.

- Aspectul este specific aplicației: nu există o bară de titlu a sistemului de operare; antetul, elementele de navigare și butoanele ferestrei sunt desenate de către aplicație și stilizate prin intermediul „temei” globale din Freya. Toate regulile de afișare (ce anume să fie prezentat, ce mesaje să i se transmită utilizatorului, către ce dispozitiv să fie trimis un element) sunt definite în `peersus-ui/src/view.rs`. Acest fișier nu depinde de niciun set de instrumente grafice (toolkit), astfel încât înlocuirea stratului de redare grafică nu necesită niciodată rescrierea regulilor.

- Versiunea mobilă are propriul layout (inseturi pentru zona sigură și poziționarea navigării); layoutul desktop nu este micșorat pur și simplu pentru un telefon.

- Utilizarea `unsafe` este permisă doar în cele două crate-uri pentru platforma Android menționate mai sus, pentru simbolurile lor de intrare și pentru transferurile de proprietate asupra descriptorilor/obiectelor JNI. Fiecare bloc `unsafe` este însoțit de un comentariu `// SAFETY:` care specifică invariantul. În toate celelalte crate-uri Rust, este setată regula `unsafe_code = "forbid"`.

- O componentă web view este permisă doar pentru conținut care este într-adevăr web (documentația API publicată și paginile wiki), niciodată ca bază a interfeței.

- Skia (Freya's renderer) este ****contruit de la sursă **** (`FORCE_SKIA_BUILD=1`), nu este preluată din arhiva precompilată neverificată. Legăturile (bindings) își preiau versiunea sursă fixată (pinned) din propriul lor fork; aceasta rămâne o decizie bazată pe încrederea în sursă. Randarea pe Linux necesită anteturi (headers) de dezvoltare pentru **C/C++ și grafică**; pe o gazdă imuabilă, utilizați containerul de compilare existent prin intermediul comenzii `./dev.sh ... --container NAME`. Mențineți opțiunea `embed-freetype` activată. Nu introduceți biblioteci de pe sistemul gazdă în container și nu ocoliți configurarea de compilare din sursă (source-build) doar pentru a reuși compilarea.

- Build UI targets on disk, not on a RAM-backed `/tmp`: set `CARGO_TARGET_DIR` to a directory on disk.

- Un build web, desktop sau Android este considerat funcțional numai după ce o persoană l-a văzut rulând. Faptul că un proces pornește nu reprezintă o acceptare.

## Validare

Înainte de a afirma că un comportament funcționează, rulează punctul real de intrare. Un test unitar trecut nu reprezintă acceptare vizuală sau end-to-end. Nu construi pachete de distribuție decât dacă ți se cere. Orice pachet modificat din original este valida si se acepta ca deviata daca serveste scopul.

- Înainte de editare, urmărește întregul traseu afectat și consumatorii existenți. Modifică sursa unică de adevăr și reutilizează componentele existente; limitează patchul la scopul cerut și verifică diff-ul real pentru duplicări, căi de cod neutilizate, alocări accidentale, expunerea datelor și modificări fără legătură.

- Alege testele în funcție de comportamentul modificat și de riscul de eșec. Acoperă tranzițiile de stare, validările, erorile și limitele de compatibilitate afectate; folosește verificările specifice existente în timpul dezvoltării și verificarea strictă aplicabilă pentru livrare. Nu înlocui testele comportamentale cu verificări ale șirurilor din sursă sau cu simple aserțiuni că o valoare nu este goală.

- Pentru o eroare, verifică traseul original care eșua, atunci când mediul permite. Separă cauzele confirmate de ipoteze. Un test pe alt dispozitiv, un rezultat de compilare sau un jurnal gol nu infirmă o problemă raportată pentru un anumit dispozitiv.

- Verifică interfața și integrarea cu platforma pe ținta reală solicitată. Testează focalizarea, introducerea textului, selecția, compunerea și trimiterea atunci când se modifică gestionarea intrării; include profilurile normale și cele pentru date secrete. Testele unitare pentru roluri sau configurare nu demonstrează comportamentul nativ al tastaturii.

- Revizuiește modificările comune în toți consumatorii și adaptoarele de platformă afectate. Constatările revizuirii sunt dovezi de evaluat, nu un motiv pentru refactorizări fără legătură sau abstractizări suplimentare. Nu impune folosirea unui agent, o revizuire la distanță sau publicarea pentru o modificare locală.

- Profilarea resurselor se realizează ****la cerere****; nu este o funcționalitate permanentă care rulează în fundal și nici o evaluare de performanță (benchmark) declanșată la fiecare modificare minoră. Efectuați o profilare specifică de tip „înainte/după” atunci când vi se solicită sau cand sunt modificari majore care ar putea afecta integritatea (la introducerea unei secvențe de cod importante sau care consumă multe resurse, ori atunci când investigați o regresie observată)

- Măsoară resursele relevante pentru acel traseu: CPU, memorie și alocări, treziri ale procesului, I/O, latența pornirii sau timpul unui cadru. Compară profiluri de build, intrări, condiții ale dispozitivului și intervale de observație echivalente; ia în calcul procesul responsabil și procesele auxiliare, dacă este relevant. Nu deduce o îmbunătățire a performanței doar din forma codului și nu compara `debug` cu `release`.

- Mențineți opțiunea de înscriere la profilare, limitată și fără conținut; opriți sondele deținute când este finalizat. Adăugați instrumentație persistentă doar pentru o nevoie concretă, nu pentru o utilizare viitoare speculativă. Preferați cea mai simplă soluție măsurată și nu slăbiți corectitudinea, agnosticismul sau mentenabilitatea pentru o optimizare nemăsurată.

- Precizează exact nivelul de verificare atins: compilat, instalat, verificat prin suma de control, comportament observat sau blocat. Installation is not proof that a reported interaction is fixed.


# Notă
#### Orice modificare este acceptabilă, cu condiția să servească unui scop valid; totuși, acest scop nu trebuie să contravină arhitecturii actuale.

- **AGENTS.md** trebuie actualizat doar dacă există modificări de arhitectură sau dacă contextul proiectului a devenit perimat în cadrul fluxului de lucru de dezvoltare/producție.
- **docs/**: strict necesar pentru menținerea documentației la zi, modificarile aplicației trebuie să reflecte codul.
- Fișierele **\*.MD** de pe GitHub trebuie actualizate doar atunci când este necesar sau la primirea unor instrucțiuni specifice.
- **GitHub CI**: este necesară aprobarea pentru orice modificare care contravine arhitecturii stabilite — cu excepția cazului în care modificarea respectivă are scopul specific de a remedia o problemă existentă.