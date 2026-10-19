[English](#english) | [Русский](#русский)

# UniDirStat

## English

A modern disk usage explorer written in Rust for **Windows, macOS, and Linux**, inspired by [WinDirStat](https://github.com/windirstat/windirstat). UniDirStat is an independent implementation and does not claim full feature parity.

Explore space usage with a linked file tree and interactive maps, find large files and exact duplicates, and refresh individual folders. Parallel scanning, English/Russian interfaces, and system, light, and dark themes are supported. Scans never follow symbolic links; cleanup uses the system trash with confirmation.

![UniDirStat — English interface, dark theme](docs/screenshots/en-dark.jpg)

[Light theme](docs/screenshots/en-light.jpg). Screenshots show the macOS app with a read-only demo snapshot, not a user's disk inventory.

### Installation

**[Download latest release](https://github.com/Vitalick/UniDirStat/releases)**

Ready-to-install releases are not published yet. Until the first release, [build from source](docs/guide.md#build-and-run). Planned release formats:

| Platform | Architecture | Release asset |
| --- | --- | --- |
| macOS | Apple Silicon | `UniDirStat-<version>-macos-arm64.dmg` |
| macOS | Intel | `UniDirStat-<version>-macos-x86_64.dmg` |
| Windows | x64 | `UniDirStat-<version>-windows-x86_64-setup.exe` |
| Linux | x64 | `UniDirStat-<version>-linux-x86_64.AppImage` |
| Debian / Ubuntu | x64 | `UniDirStat-<version>-linux-x86_64.deb` |

Open the macOS DMG and drag UniDirStat into Applications; on Windows, run the installer. On Linux, make the AppImage executable (`chmod +x UniDirStat-*.AppImage`) and run it, or install the Debian package with `sudo apt install ./UniDirStat-*.deb`. Native Linux file dialogs require `xdg-desktop-portal` and a backend for your desktop.

macOS builds use local ad-hoc signing and are not notarized; Windows installers are unsigned. The OS may show an unverified-publisher warning. Do not disable system security protections. The in-app update check is currently a placeholder and makes no network requests.

### Documentation

[Full guide](docs/guide.md): features, GUI and shortcuts, CLI, size accounting, safety and limitations, building from source, checks, and releases. [Translation guide](docs/localization.md).

### Support development

If UniDirStat is useful to you, you can [support development with a voluntary contribution of any amount](https://app.lava.top/4026101527). Support is not a condition of using the software and does not include any promise of paid services.

### Stars

If you find UniDirStat useful, [give it a star on GitHub](https://github.com/Vitalick/UniDirStat). [Star History](https://www.star-history.com/#Vitalick/UniDirStat&Date).

**License:** [GPL-3.0-or-later](LICENSE). [Attribution and font license](docs/guide.md#license).

---

## Русский

Современный анализатор дискового пространства на Rust для **Windows, macOS и Linux**, вдохновлённый [WinDirStat](https://github.com/windirstat/windirstat). UniDirStat — самостоятельная реализация, без заявления о полном совпадении функций.

Изучайте занятое место через связанное дерево файлов и интерактивные карты, находите крупные файлы и точные дубликаты, пересчитывайте отдельные папки. Есть параллельное сканирование, русский и английский интерфейсы, системная, светлая и тёмная темы. Сканирование не следует по символическим ссылкам; очистка использует системную корзину с подтверждением.

![UniDirStat — русский интерфейс, тёмная тема](docs/screenshots/ru-dark.jpg)

[Светлая тема](docs/screenshots/ru-light.jpg). На снимках — приложение на macOS с демонстрационным снимком только для чтения, а не с данными пользовательского диска.

### Установка

**[Скачать последнюю версию](https://github.com/Vitalick/UniDirStat/releases)**

Готовые релизы пока не опубликованы. До первого выпуска [соберите приложение из исходников](docs/guide.ru.md#сборка-и-запуск). Форматы будущих релизов:

| Платформа | Архитектура | Файл релиза |
| --- | --- | --- |
| macOS | Apple Silicon | `UniDirStat-<version>-macos-arm64.dmg` |
| macOS | Intel | `UniDirStat-<version>-macos-x86_64.dmg` |
| Windows | x64 | `UniDirStat-<version>-windows-x86_64-setup.exe` |
| Linux | x64 | `UniDirStat-<version>-linux-x86_64.AppImage` |
| Debian / Ubuntu | x64 | `UniDirStat-<version>-linux-x86_64.deb` |

На macOS откройте DMG и перетащите UniDirStat в Applications; на Windows запустите установщик. На Linux сделайте AppImage исполняемым (`chmod +x UniDirStat-*.AppImage`) и запустите его либо установите Debian-пакет командой `sudo apt install ./UniDirStat-*.deb`. Для системных диалогов Linux нужны `xdg-desktop-portal` и backend вашего рабочего стола.

Сборки macOS подписаны локально ad hoc, без нотариализации; установщики Windows не подписаны. ОС может предупредить о непроверенном издателе. Не отключайте системные средства защиты. Проверка обновлений в программе пока является заглушкой и не обращается к сети.

### Документация

[Полное руководство](docs/guide.ru.md): возможности, интерфейс и сочетания клавиш, CLI, учёт размера, безопасность и ограничения, сборка из исходников, проверки и релизы. [Руководство по локализации](docs/localization.md) — на английском.

### Поддержать разработку

Если UniDirStat оказался полезен, можно [добровольно поддержать разработку любой суммой](https://app.lava.top/4026101527). Поддержка не является условием использования программы и не подразумевает обещания платных услуг.

### Звёзды

Если UniDirStat полезен, [поставьте звезду на GitHub](https://github.com/Vitalick/UniDirStat). [История звёзд](https://www.star-history.com/#Vitalick/UniDirStat&Date).

**Лицензия:** [GPL-3.0-or-later](LICENSE). [Атрибуция и лицензия шрифта](docs/guide.ru.md#лицензия).
