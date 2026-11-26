# IMPORTANT

## PR #19 și #20 devin trimiteri reale de pluginuri noi

PR #19 și #20 au fost deschise ca probă a verificării de resurse din CI și s-au închis automat când `main` a fost înlocuit cu un commit nou fără istoric comun. Ele nu rămân probe închise: când proprietarul are pluginuri noi, primul plugin nou se trimite prin #19, al doilea prin #20.

### Ce permite GitHub

- Ramura unui PR (`head`) nu se poate schimba niciodată. #19 și #20 au amândouă ramura `demo/ci-benchmark-slowdown`, aceeași ca PR #21.
- Conținutul ramurii se poate schimba oricând; PR-ul arată ce conține ramura.
- Baza (`base`) se poate schimba doar cât PR-ul este deschis.
- Nu pot exista două PR-uri deschise cu aceeași ramură și aceeași bază. Din același motiv, #19 și #20 se folosesc pe rând, nu în același timp.
- Commiturile cu care s-au închis rămân păstrate de GitHub în `refs/pull/19/head` (`503680f`) și `refs/pull/20/head` (`0c3935a`).
- CI-ul de extensii (`extension-validate.yml`) planifică trimiterea după commiturile de bază și de vârf, nu după numele ramurii, deci o ramură care nu se numește `submission/<id>-<dată>` este verificată la fel.

### Procedura, pentru fiecare plugin nou

1. Niciun alt PR deschis nu folosește `demo/ci-benchmark-slowdown` (PR #21 este închis).
2. Ramura revine exact la commitul cu care s-a închis PR-ul (#19: `git fetch origin refs/pull/19/head` și `git push --force-with-lease origin FETCH_HEAD:refs/heads/demo/ci-benchmark-slowdown`).
3. PR-ul se redeschide (`gh pr reopen 19`).
4. Cât este deschis, baza devine `extension` (`gh pr edit 19 --base extension`).
5. Ramura primește trimiterea pluginului: un commit pe vârful lui `extension`, care atinge doar `extension/<nume>/`, la fel ca o ramură `submission/<id>-<dată>`.
6. Titlul și descrierea devin cele ale trimiterii, conform șablonului de PR pentru extensii; eticheta `run:ci` pornește verificarea.
7. După ce proprietarul acceptă trimiterea, ramura este liberă și #20 urmează aceeași procedură pentru al doilea plugin.

Pașii 2–3 nu au fost încercați încă: redeschiderea lui #19 a fost refuzată după ce ramura fusese mutată. Prima folosire confirmă procedura; dacă GitHub refuză și după pasul 2, se notează aici motivul exact.

Un PR cu baza `extension` nu se mai închide la înlocuirea lui `main`.

## Nu înlocui `main` cât un PR țintește `main`

Fiecare publicare înlocuiește `main` cu un commit-rădăcină nou. GitHub închide atunci, automat și definitiv, orice PR deschis cu baza `main`. Înainte de `scripts/deploy.py dev --publish` se verifică `gh pr list --base main --state open`: dacă lista nu e goală, nu se publică. O reparație pentru un PR deschis se urcă pe ramura acelui PR, iar CI-ul rulează din nou pe același PR.
